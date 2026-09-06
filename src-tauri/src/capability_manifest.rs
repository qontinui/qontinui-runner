//! The capability manifest — for every capability this binary delivers, WHICH
//! RUNG answered.
//!
//! Plan: `2026-08-31-published-build-parity-check`, Phase 1 (the rung
//! vocabulary and the renderer; Phases 2-7 wire the observations, the doors and
//! the comparator).
//!
//! # The question this answers, and why nothing answered it before
//!
//! `GET :9876/health` says *what this binary is* (`gitSha`, `buildId`,
//! `mainSha`, `buildDrift`). [`crate::coord_doctor`] says *whether this runner
//! can set gates*. [`qontinui_runner_lib::config_report`] says *where each
//! configuration value came from*. **No surface says what this binary can
//! actually DO here** — and that is precisely the question an external operator
//! running the published installer needs answered, because several of the
//! runner's assets are resolved through **rung-ordered resolvers that try a
//! developer checkout before falling back**. On the author's machine the
//! checkout rung answers; on an operator's machine it does not, and the
//! fallback either silently differs or is absent.
//!
//! [`crate::bundled_resources`] states the principle this module operationalises:
//! *"a file shipped inside the installer and a file that exists only in a
//! developer checkout are different things, and resolving one as the other is a
//! wrong answer that looks right on the author's machine."* This module makes
//! that difference a **value** rather than a property of whose laptop is running.
//!
//! # Why this lives in the BIN crate, unlike `config_report`
//!
//! [`qontinui_runner_lib::config_report`] lives in the lib because it has two
//! consumers (a headless `config_report` bin and an in-app Tauri command), and it
//! pays for that with documented blindness: ten of its fifteen layers live in
//! bin-only modules and report `Unknown` from the headless bin.
//!
//! This module makes the opposite trade, deliberately, and the plan's Design
//! decision 1 is explicit about it: **the manifest is emitted by the shipped app
//! binary `qontinui-runner.exe`, not by a twelfth helper bin.**
//! `tauri.conf.json`'s `bundle.externalBin` ships exactly two sidecars
//! (`qontinui_profile`, `qontinui-pr`); a new `[[bin]]` would not be in the
//! installer, so it could not answer the one question that matters. It would
//! also inherit `config_report`'s blindness — which here is *most of the subject
//! matter*: every capability in [`CAPABILITY_SPECS`] is resolved by a bin-only
//! module ([`crate::bundled_resources`], [`crate::workspace_paths`],
//! [`crate::spec_api`], [`crate::fleet_commands`], [`crate::fleet_skills`],
//! [`crate::fleet_agents`], [`crate::agent_runtime`],
//! [`crate::agent_commands`], [`crate::slash_commands`]). A lib-side copy could
//! not call a single one of them, and `impl From<crate::agent_commands::CommandSource>`
//! would not even compile there.
//!
//! # [`Rung`] is a SUPERSET, not a fifth vocabulary
//!
//! This codebase already answers "which rung answered?" in five places, and a
//! sixth parallel enum would be the drift trap those five exist to avoid. So
//! [`Rung`] is defined as the union that the existing provenance types **map
//! into**, with a total conversion from each:
//!
//! | Existing provenance | Owner | Conversion here |
//! |---|---|---|
//! | `WorkspaceRootKind` (`Declared`/`Discovered`/`HomeDefault`/`Unresolved`) | `qontinui_types::paths` | [`impl From<WorkspaceRootKind> for Rung`](#impl-From<WorkspaceRootKind>-for-Rung) |
//! | `CommandSource` (`Builtin`/`Served`/`DiskCache`) | [`crate::agent_commands`] | [`impl From<CommandSource> for Rung`](#impl-From<CommandSource>-for-Rung) |
//! | `SkillSource` (`Builtin`/`User`/`Community`/`Other`) | [`crate::skills`] | [`rung_for_skill_source`] |
//! | `ProbeScopeKind` | [`crate::env_agent::collectors`] | *(scope of a toolchain probe, not an asset rung — deliberately NOT converted; see below)* |
//! | `embedded_pg::db_arm()` → `/health` `database.arm` | [`crate::embedded_pg`] | *(cited as the shape precedent; the DB arm is not an asset rung)* |
//!
//! `ProbeScopeKind` and `db_arm` are cited rather than converted **on purpose**.
//! They answer "which tree did the version probes run in?" and "which Postgres
//! is this process on?" — neither is "where did this binary find an asset it
//! delivers", and forcing them through this vocabulary would produce a row whose
//! rung is a category error. Their SHAPE is what is copied: a `Copy` enum with a
//! [`wire`](Rung::wire) returning a stable snake_case `&'static str`, plus an
//! explicit `Unresolved` variant so an absence is a *stated* kind rather than a
//! missing row. That shape rule is `WorkspaceRootKind`'s own, quoted from its
//! docs: *"Carried so an unresolved root is a stated kind rather than an absent
//! one — the same absence-is-not-a-value rule the rest of this module follows."*
//!
//! # `rejected` is load-bearing, and is carried EVEN ON SUCCESS
//!
//! `WorkspaceRoot::rejected` reports *which higher-priority candidate was
//! present but unusable* — **reported even when the resolution succeeded**, so
//! the fall-through is never invisible. [`CapabilityRow::rejected`] carries that
//! through, because it is the only field that distinguishes *"the dev checkout
//! answered"* from *"the bundle was there and was rejected, so the dev checkout
//! answered"*. Those two look identical in the `rung` alone and are completely
//! different findings: the first is normal on a dev box, the second is a bundle
//! defect that a published install would hit as `Unresolved`.
//!
//! # The three things this module refuses to do
//!
//! Inherited verbatim from [`qontinui_runner_lib::config_report`], whose module
//! header states them first:
//!
//! 1. **It never renders an unobservable row as the value the code would have
//!    fallen back to.** A capability nothing observed is [`Rung::Unknown`], and
//!    its [`note`](CapabilityRow::note) **names the symbol that owns it** so the
//!    reader goes to the right place. Reporting "this resolved via `Embedded`"
//!    when the truth is "nothing looked" is worse than printing nothing: the
//!    manifest's whole job is to be diffed, and a fabricated row diffs clean.
//! 2. **It never omits a row it could not resolve.** [`build_manifest`] walks
//!    [`CAPABILITY_SPECS`] and emits one row per spec, unconditionally. A
//!    silently-dropped row reads as a clean bill of health, which is the exact
//!    failure this plan exists to end.
//! 3. **It never re-derives a resolution order.** Every rung in a row is
//!    supplied BY the resolver that produced the value (Phase 2 and Phase 3 grow
//!    those return shapes). A second copy of a precedence rule compiles, agrees
//!    with the real one the day it is written, and silently starts lying the
//!    first time the real one changes — reproducing, inside the diagnostic,
//!    exactly the class of defect the diagnostic exists to expose.
//!
//! # `schema_version` is emitted from the first commit
//!
//! [`SCHEMA_VERSION`] starts at 1 and ships in every manifest, including this
//! phase's all-`Unknown` one. The comparator's whole job is diffing two builds,
//! and two builds far enough apart will eventually differ in the manifest
//! *format* as well as its contents — at which point a comparator with no
//! version field either mis-diffs or has to guess. The discipline is
//! [`crate::agent_commands`]'s `CACHE_VERSION`, whose rule is *"a cache written
//! by a different version is ignored rather than parsed on a guess."*
//!
//! # What Phase 3 added: the provisioning ledger
//!
//! Phase 1 shipped the roster, the vocabulary, the conversions, the renderer and
//! the tests, and every row it produced was [`Rung::Unknown`] — the honest state
//! for a module where nothing had looked yet. Phase 3 makes the
//! `session_provisioning` and `served_registry` rows real, by giving every
//! `provision_*` path a [`ProvisionReport`] to return.
//!
//! **The fail-soft posture those paths have is unchanged and must stay
//! unchanged.** A spawn that cannot provision its commands, its skills or its
//! subagent definitions still succeeds; nothing added here can abort one. What
//! changed is that the degradation is a VALUE — `written` out of `expected`,
//! every skipped unit WITH its [`SkipReason`], and the [`Rung`] that answered —
//! instead of a `warn!` in a log file an external operator will never read. The
//! reports land in a bounded per-session ledger ([`record_provision`],
//! [`session_provision_ledger`]) and in a per-capability latest-observation
//! index that [`ManifestInputs::observed`] reads straight into
//! [`build_manifest`].
//!
//! Phase 3 also closed the one caveat Phase 1 had to carry: `CommandSource` was
//! widened from `Builtin`/`Account` to `Builtin`/`Served`/`DiskCache` so it has
//! one variant per arm of `agent_commands::resolve_registry`, and
//! `skills::SkillDefinition.source` became the typed
//! [`crate::skills::SkillSource`] rather than a bare `String`. See
//! [`Rung::from_command_source`] and [`rung_for_skill_source`].
//!
//! Rows still owned by Phases 2 and 4-7 remain `Unknown`, naming their anchors.

use qontinui_types::paths::WorkspaceRootKind;
use serde::{Serialize, Serializer};

use crate::agent_commands::CommandSource;
use crate::skills::SkillSource;

/// Wire-format version of [`CapabilityManifest`].
///
/// Bumped when the manifest's SHAPE changes — a field added, removed or
/// re-typed, or a [`Rung`] wire string changed. Not bumped when a
/// [`CapabilitySpec`] row is added: that is a content change the comparator
/// already handles (a row present in one manifest and absent in the other is
/// one of the three differences Phase 5 reports).
///
/// A consumer reading a manifest whose `schema_version` it does not know must
/// refuse it rather than parse it on a guess — the rule
/// [`crate::agent_commands`]'s `CACHE_VERSION` states for its own cache.
pub const SCHEMA_VERSION: u32 = 1;

// ===========================================================================
// The rung vocabulary.
// ===========================================================================

/// WHICH RUNG answered for one capability.
///
/// The union of every "where did this come from?" vocabulary already in this
/// codebase, ordered from *carried by the build* down to *found on the
/// operator's disk* and then to the two non-answers. The ordering is meaningful
/// and is the reason the parity check works at all: a row that resolves near the
/// TOP of this list resolves the same way on every machine, and a row that
/// resolves near the BOTTOM resolves only where a checkout happens to exist.
///
/// A dev build reporting [`DevCheckout`](Self::DevCheckout) or
/// [`OperatorCheckout`](Self::OperatorCheckout) where a published install
/// reports [`Unresolved`](Self::Unresolved) **is** a parity defect, and nobody
/// had to predict which capability it would be.
///
/// `Copy` + [`wire`](Self::wire) is the house shape, taken from
/// `qontinui_types::paths::WorkspaceRootKind` and
/// [`crate::env_agent::collectors`]'s `ProbeScopeKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rung {
    /// Compiled into this binary — `include_str!` / `include_dir!`. Present on
    /// every machine that has the binary, by construction. This is the rung a
    /// capability wants to be on if it wants to be portable.
    Embedded,
    /// Unpacked from the installer's `bundle.resources` and located through
    /// Tauri's `BaseDirectory::Resource`. Ships with the installer, but — unlike
    /// [`Embedded`](Self::Embedded) — can be absent if the bundle declaration
    /// does not list the file, which is a defect invisible on a dev box.
    BundleResource,
    /// Fetched over the network from qontinui-web or coord at run time. Portable
    /// in principle; unavailable to an offline or unpaired operator, which is a
    /// different failure from "missing".
    Served,
    /// Read from a store this device wrote for itself — the runner's on-disk
    /// override cache, or its local database. Device-local: never carried by the
    /// build, and populated only by an earlier successful `Served` resolution.
    DiskCache,
    /// Located relative to `std::env::current_exe()` — for a dev build,
    /// `<repo>/src-tauri/target/<profile>/`, so the checkout the exe was built
    /// in. Answers on a dev box and on nothing else.
    ExeRelativeCheckout,
    /// Located under `<workspace-root>/qontinui-runner/src-tauri/…` — THIS
    /// repo's source tree, found via the workspace root rather than via the exe.
    /// Answers only where a `qontinui-runner` checkout exists.
    DevCheckout,
    /// Located under a SIBLING repo checkout on the operator's disk —
    /// `<workspace-root>/qontinui-claude-config/.claude/…` and friends. Answers
    /// only on a machine that has that other repo, which an external operator
    /// has no reason to.
    OperatorCheckout,
    /// Every rung was tried and none answered. **A stated outcome, not a missing
    /// row** — the same absence-is-not-a-value rule `WorkspaceRootKind`
    /// documents for its own `Unresolved`.
    Unresolved,
    /// Nothing looked. The capability was **not observed from here**, and the
    /// row's [`note`](CapabilityRow::note) names the symbol that owns the
    /// resolution.
    ///
    /// This is not a synonym for [`Unresolved`](Self::Unresolved) and the two
    /// must never be collapsed: `Unresolved` is a finding about the machine
    /// ("every rung missed"), `Unknown` is a finding about the OBSERVER ("no
    /// resolver reported here"). Treating the second as the first would blame a
    /// published install for a gap in this module.
    Unknown,
}

impl Rung {
    /// Every variant, in declaration order. Used by the doc renderer and by the
    /// totality tests; a new variant must be added here, which the
    /// `rung_all_covers_every_variant` test enforces via an exhaustive match.
    pub const ALL: &'static [Rung] = &[
        Rung::Embedded,
        Rung::BundleResource,
        Rung::Served,
        Rung::DiskCache,
        Rung::ExeRelativeCheckout,
        Rung::DevCheckout,
        Rung::OperatorCheckout,
        Rung::Unresolved,
        Rung::Unknown,
    ];

    /// The stable snake_case wire string. This is the value that appears in the
    /// JSON manifest and that Phase 5's comparator diffs, so it is a contract:
    /// changing one is a [`SCHEMA_VERSION`] bump.
    ///
    /// Mirrors `WorkspaceRootKind::wire` and `ProbeScopeKind::wire`.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Rung::Embedded => "embedded",
            Rung::BundleResource => "bundle_resource",
            Rung::Served => "served",
            Rung::DiskCache => "disk_cache",
            Rung::ExeRelativeCheckout => "exe_relative_checkout",
            Rung::DevCheckout => "dev_checkout",
            Rung::OperatorCheckout => "operator_checkout",
            Rung::Unresolved => "unresolved",
            Rung::Unknown => "unknown",
        }
    }

    /// One line explaining the rung, shared by the generated doc and the text
    /// render so the two cannot drift.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Rung::Embedded => {
                "compiled into the binary (`include_str!` / `include_dir!`) — present \
                 wherever the binary is"
            }
            Rung::BundleResource => {
                "unpacked from the installer's `bundle.resources` and located via \
                 Tauri's `BaseDirectory::Resource`"
            }
            Rung::Served => "fetched over the network from qontinui-web or coord at run time",
            Rung::DiskCache => {
                "read from a store this device wrote for itself (the on-disk override \
                 cache, or the local database) — never carried by the build"
            }
            Rung::ExeRelativeCheckout => {
                "found relative to `current_exe()`, i.e. the checkout this binary was \
                 built in — answers on a dev box and nowhere else"
            }
            Rung::DevCheckout => {
                "found under `<workspace-root>/qontinui-runner/src-tauri/…` — this \
                 repo's source tree, via the workspace root rather than the exe"
            }
            Rung::OperatorCheckout => {
                "found under a SIBLING repo checkout on the operator's disk (e.g. \
                 `qontinui-claude-config`) — answers only where that repo exists"
            }
            Rung::Unresolved => "every rung was tried and none answered — a stated outcome",
            Rung::Unknown => {
                "nothing observed this capability here — a finding about the OBSERVER, \
                 never about the machine"
            }
        }
    }

    /// True when this rung represents an actual, working resolution.
    ///
    /// [`Unresolved`](Self::Unresolved) and [`Unknown`](Self::Unknown) are the
    /// two that are not, for different reasons, and
    /// [`CapabilityManifest::unresolved_count`] counts both — see its docs for
    /// why lumping them in the COUNT while keeping them distinct in the ROWS is
    /// the right split.
    #[must_use]
    pub fn is_resolved(self) -> bool {
        match self {
            Rung::Embedded
            | Rung::BundleResource
            | Rung::Served
            | Rung::DiskCache
            | Rung::ExeRelativeCheckout
            | Rung::DevCheckout
            | Rung::OperatorCheckout => true,
            Rung::Unresolved | Rung::Unknown => false,
        }
    }

    /// This rung's position in [`ALL`](Self::ALL): `0` is the most portable
    /// answer, and the two non-answers rank last.
    ///
    /// The ordering is not invented here — it is [`ALL`](Self::ALL)'s own,
    /// stated in this enum's docs (*"ordered from carried by the build down to
    /// found on the operator's disk and then to the two non-answers"*). Exposed
    /// so a caller that probed SEVERAL assets for one capability row can report
    /// the WORST of them rather than the first, which is the conservative
    /// reading: a row that claims `bundle_resource` while one of its assets only
    /// resolved from a checkout would hide exactly the parity defect this
    /// manifest exists to surface.
    #[must_use]
    pub fn rank(self) -> usize {
        // `ALL` is total (`rung_all_covers_every_variant`), so the fallback is
        // unreachable; it is a saturating "worst" rather than a panic because a
        // diagnostic must never abort while reporting.
        Rung::ALL
            .iter()
            .position(|r| *r == self)
            .unwrap_or(Rung::ALL.len())
    }

    /// True when this rung answers only where a repo checkout happens to exist —
    /// the rungs whose presence on a dev box and absence on a published install
    /// IS the parity class this plan measures.
    #[must_use]
    pub fn is_checkout_bound(self) -> bool {
        match self {
            Rung::ExeRelativeCheckout | Rung::DevCheckout | Rung::OperatorCheckout => true,
            Rung::Embedded
            | Rung::BundleResource
            | Rung::Served
            | Rung::DiskCache
            | Rung::Unresolved
            | Rung::Unknown => false,
        }
    }
}

/// Serialized as its [`wire`](Rung::wire) string, by delegation rather than by a
/// `rename_all` attribute — one source of truth, so the JSON and the text render
/// cannot drift apart.
impl Serialize for Rung {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire())
    }
}

// ===========================================================================
// Conversions FROM the provenance vocabularies that already exist.
//
// Every one of these is an exhaustive `match` with no `_` arm ON PURPOSE:
// adding a variant upstream must break this build, because a silently-defaulted
// new variant is exactly the kind of "looks right on the author's machine"
// answer this module exists to refuse.
// ===========================================================================

impl From<WorkspaceRootKind> for Rung {
    /// Map `qontinui_types::paths::WorkspaceRootKind` into the manifest
    /// vocabulary.
    ///
    /// - `Declared` → [`Rung::OperatorCheckout`]. An explicit `$QONTINUI_ROOT`,
    ///   its alias, or the runner's own `paths.workspace_root` setting: the
    ///   operator stated where their checkouts live, so what answered is a
    ///   checkout on their disk.
    /// - `Discovered` → [`Rung::ExeRelativeCheckout`]. The ancestor walk, and
    ///   for this consumer the anchor is specifically `current_exe()` —
    ///   [`crate::workspace_paths::runner_workspace_root_from`] passes
    ///   `exe_anchor(std::env::current_exe().ok())` and nothing else — so
    ///   `Discovered` here is exactly "found relative to where this exe sits".
    /// - `HomeDefault` → [`Rung::OperatorCheckout`]. `<home>/qontinui-root`, the
    ///   portable last-resort convention. Still a checkout on the operator's
    ///   disk; the fact that convention rather than declaration found it is
    ///   preserved in the row's `detail`, not in the rung.
    /// - `Unresolved` → [`Rung::Unresolved`], one to one.
    ///
    /// **Two upstream variants collapse onto `OperatorCheckout`, and that loss
    /// is deliberate but must not be silent.** Callers building a row from a
    /// `WorkspaceRoot` are required to put the upstream `kind.wire()` in
    /// [`CapabilityRow::detail`] — [`CapabilityObservation::from_workspace_root_kind`]
    /// does it for them — so the manifest carries both the comparable rung and
    /// the exact upstream verdict.
    fn from(kind: WorkspaceRootKind) -> Rung {
        match kind {
            WorkspaceRootKind::Declared => Rung::OperatorCheckout,
            WorkspaceRootKind::Discovered => Rung::ExeRelativeCheckout,
            WorkspaceRootKind::HomeDefault => Rung::OperatorCheckout,
            WorkspaceRootKind::Unresolved => Rung::Unresolved,
        }
    }
}

impl From<CommandSource> for Rung {
    /// Map [`crate::agent_commands::CommandSource`] into the manifest
    /// vocabulary — one variant to one rung, with nothing collapsed and nothing
    /// guessed. See [`Rung::from_command_source`].
    fn from(source: CommandSource) -> Rung {
        Rung::from_command_source(source).0
    }
}

impl Rung {
    /// [`From<CommandSource>`], plus the slot a caveat would occupy if this
    /// conversion had one. **It no longer does**, and the tuple is kept so the
    /// two callers ([`From`] and
    /// [`CapabilityObservation::from_command_source`]) share one mapping rather
    /// than two that can drift.
    ///
    /// # The mapping, and why it is now exact
    ///
    /// [`crate::agent_commands`]'s `resolve_registry()` has **three** arms —
    /// fetch from the account (`fetch_overrides_blocking`), else the on-disk
    /// cache (`read_cache_at`, `agent-commands-cache.json`), else the embedded
    /// default. Phase 1 of this plan mapped `Account → Unknown` with a caveat
    /// note, because `CommandSource` then had only **two** variants and
    /// `Account` covered the first two arms indistinguishably: a body off the
    /// wire and a body out of the cache were the same value.
    ///
    /// That distinction is not a nicety here; it is the measurement. **A
    /// published install with no network resolves cached-or-embedded where a
    /// dev box resolves served**, and reporting both as one rung erases exactly
    /// the parity difference this manifest exists to surface. Phase 3 split the
    /// upstream type instead of teaching this function to guess, so the
    /// conversion is now one-to-one and the caveat is gone:
    ///
    /// - `Builtin` → [`Rung::Embedded`] (`include_str!`; present wherever the
    ///   binary is).
    /// - `Served` → [`Rung::Served`] (fetched over the network this run).
    /// - `DiskCache` → [`Rung::DiskCache`] (this device's own cache file).
    ///
    /// Exhaustive with no `_` arm, deliberately: a variant added upstream must
    /// break this build rather than silently default.
    #[must_use]
    pub fn from_command_source(source: CommandSource) -> (Rung, Option<&'static str>) {
        match source {
            CommandSource::Builtin => (Rung::Embedded, None),
            CommandSource::Served => (Rung::Served, None),
            CommandSource::DiskCache => (Rung::DiskCache, None),
        }
    }
}

/// Map [`crate::skills::SkillSource`] into the manifest vocabulary.
///
/// This took a `&str` in Phase 1, because `SkillDefinition.source` was a bare
/// `String` — the one place in this crate where the provenance pattern had
/// decayed into free text, so the manifest had to parse a string to answer a
/// question the type system should hold. Phase 3 typed the field; this function
/// now matches on a type, exhaustively and with no `_` arm.
///
/// - [`SkillSource::Builtin`] → [`Rung::Embedded`]. **Verified**, not assumed:
///   `skills::BUILTIN_SKILLS_JSON` is `include_str!("builtin.json")`.
/// - [`SkillSource::User`] → [`Rung::DiskCache`]. Loaded by
///   `skills::SkillRegistry::with_pg` / `PgDb::list_user_skills` from this
///   device's own database. Not a *file* cache, but the property the rung names
///   is the one that matters here: device-local state the runner itself wrote,
///   never carried by the build.
/// - [`SkillSource::Community`] → [`Rung::Unknown`], and the reason is **not**
///   the one Phase 1 recorded. Phase 1 stated that this value is "declared in
///   the type's own comment and produced by no code path"; that is **false
///   against the real code** — `mcp::skills::sync_pull` fetches an
///   organization's skills over HTTP, stamps `source = community` on each, and
///   `PgDb::import_skills` then persists them into the same local `user_skills`
///   table every `user` row lives in. So the value IS produced, and it names
///   **two different rungs at once**: [`Rung::Served`] for where the row was
///   authored, [`Rung::DiskCache`] for where this binary reads it back from.
///   One field cannot state both, and picking either would report a fact this
///   function did not observe — so it reports neither. `Unknown` is a finding
///   about the OBSERVER, which is exactly what this is.
/// - [`SkillSource::Other`] → [`Rung::Unknown`]. A value no producer in this
///   binary emits (it survives a round trip verbatim rather than being coerced)
///   is never guessed at.
///
/// Values are matched **case-sensitively and untrimmed** by
/// [`SkillSource::from_wire`], because they are wire values written by
/// producing code, not operator input — normalising would quietly accept a
/// shape no producer emits and hide a real drift.
#[must_use]
pub fn rung_for_skill_source(source: &SkillSource) -> Rung {
    match source {
        SkillSource::Builtin => Rung::Embedded,
        SkillSource::User => Rung::DiskCache,
        // Produced by `mcp::skills::sync_pull` + `PgDb::import_skills`, and
        // straddling `served` (authored) and `disk_cache` (read back here).
        SkillSource::Community => Rung::Unknown,
        SkillSource::Other(_) => Rung::Unknown,
    }
}

// ===========================================================================
// The capability inventory — a static data table, in the LAYER_SPECS /
// CHECK_SPECS style.
//
// This table is the SINGLE source of truth for: the capability roster, its
// order, the built manifest, the JSON, and the generated markdown doc
// (`render_manifest_doc`). Adding a capability means adding a row here and
// wiring its observation — nothing else structural.
// ===========================================================================

/// The static spec for one capability.
///
/// `anchor` is the field that makes discipline (1) of the module header
/// mechanically possible: a row nothing observed renders `Unknown` **naming the
/// symbol that owns it**, and it can only do that because the symbol is data
/// rather than a comment. Deliberately a SYMBOL and not a line number — this
/// plan's sibling (`config_report`) recorded its own anchors drifting 66 commits
/// between vet and implement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CapabilitySpec {
    /// Stable machine identifier. Appears in the JSON, the text render and the
    /// generated doc, and is the key Phase 5's comparator joins two manifests
    /// on — so it is a contract, not a label.
    pub id: &'static str,
    /// Coarse grouping, for reading the doc rather than for logic. One of
    /// `path_resolution`, `bundled_asset`, `session_provisioning`,
    /// `served_registry`.
    pub class: &'static str,
    /// What this capability IS, and what its absence costs an operator. Written
    /// for someone reading the generated document with no other context.
    pub description: &'static str,
    /// The rungs this capability is expected to be able to answer on, best
    /// first. **Not an assertion and not a gate** — a row resolving off this
    /// list is reported, never rejected. Its purpose is to let a reader (and
    /// Phase 5) see at a glance which capabilities are checkout-bound by
    /// design, and therefore which ones a published install is expected to
    /// answer differently.
    pub expected_rungs: &'static [Rung],
    /// The SYMBOL that performs the resolution. Named in the `Unknown` note when
    /// nothing observed this row, so a reader goes straight to the code rather
    /// than to a guess.
    pub anchor: &'static str,
}

/// The capabilities this binary delivers whose resolution is rung-ordered, in
/// manifest order.
///
/// Every row here was read out of the code it names, not out of the plan: the
/// plan's Phase 1 named a minimum roster, and each entry's `anchor`,
/// `expected_rungs` and `description` were checked against `origin/main` at
/// implementation time.
///
/// **This roster is incomplete by construction and says so.** A manifest reports
/// only what someone thought to list; that is the plan's own stated weakness of
/// this shape (Shape A: *"it cannot catch a capability nobody enumerated"*), and
/// it is why the plan pairs it with a behavioural second axis (Phase 6) whose
/// disagreements are reported as findings **about the roster**. Adding a
/// capability is adding a row here.
pub const CAPABILITY_SPECS: &[CapabilitySpec] = &[
    CapabilitySpec {
        id: "workspace_root",
        class: "path_resolution",
        description: "Where the Qontinui repo checkouts live on this box. This is the \
                      root every checkout-bound capability below is resolved relative \
                      to, so it is the row that explains most of the others: when it is \
                      `unresolved`, every `dev_checkout` and `operator_checkout` row \
                      downstream of it is unresolved too, and the manifest should be \
                      read from this row outward. A published install on an operator's \
                      machine is EXPECTED to have no workspace root at all — that is \
                      the normal case for the audience, not a fault.",
        expected_rungs: &[
            Rung::OperatorCheckout,
            Rung::ExeRelativeCheckout,
            Rung::Unresolved,
        ],
        anchor: "workspace_paths::runner_workspace_root → qontinui_types::paths::qontinui_workspace_root",
    },
    CapabilitySpec {
        id: "bundled_resources",
        class: "bundled_asset",
        description: "Crate-bundled assets resolved at run time — \
                      `resources/code-semantics/ts-language-service.mjs` and everything \
                      under `data/` (`runner_state_machine.json`, `htn_methods/`). The \
                      resolver tries the installer's unpacked resource dir, then the \
                      checkout the exe was built in, then the workspace-root copy of \
                      this repo. Its own module doc names the failure this row exists to \
                      catch: resolving a developer-checkout file as a shipped one is \
                      \"a wrong answer that looks right on the author's machine\".",
        expected_rungs: &[
            Rung::BundleResource,
            Rung::ExeRelativeCheckout,
            Rung::DevCheckout,
            Rung::Unresolved,
        ],
        anchor: "bundled_resources::resolve_with_rung over resolve / exe_relative_checkout / dev_checkout",
    },
    CapabilitySpec {
        id: "spec_pages",
        class: "bundled_asset",
        description: "The UI Bridge page specs (IR, projection, notes) that back the \
                      spec API. Read FILESYSTEM-FIRST from a repo root the caller \
                      supplies, with the compile-time `EMBEDDED_PAGES` snapshot as the \
                      fallback — and the embedded snapshot covers the `qontinui-runner` \
                      app only, so for any other app the filesystem rung is the only \
                      rung there is. Filesystem-first means a dev box silently reads a \
                      DIFFERENT corpus from the one an operator gets.",
        // No `DevCheckout`: Phase 2's mapping emits `OperatorCheckout` for the
        // filesystem arm, because the caller's `root` is any REGISTERED app's
        // `<repo_root>/specs` (`resolve_specs_root`), not specifically
        // `<workspace-root>/qontinui-runner/src-tauri/…`. Listing a rung this
        // capability cannot emit would read as an unmet expectation forever.
        expected_rungs: &[
            Rung::OperatorCheckout,
            Rung::Embedded,
            Rung::Unresolved,
        ],
        anchor: "spec_api::storage::{read_ir, read_projection, read_notes, list_pages} over EMBEDDED_PAGES",
    },
    CapabilitySpec {
        id: "fleet_commands",
        class: "session_provisioning",
        description: "The agent command procedures written into a spawned session's \
                      `<cwd>/.claude/commands/*.md`, so `/vet-plan` and friends resolve \
                      in a session whose cwd is a fresh worktree. Embedded via \
                      `include_str!`, so it should answer on every machine — and every \
                      failure path here degrades one step and `warn!`s rather than \
                      aborting the spawn, which is correct behaviour and completely \
                      invisible. This row is what makes that degradation a value.",
        expected_rungs: &[Rung::Embedded, Rung::Unresolved],
        anchor: "fleet_commands::provision_fleet_commands_into over FLEET_COMMANDS",
    },
    CapabilitySpec {
        id: "fleet_skills",
        class: "session_provisioning",
        description: "The agent SKILLS written into a spawned session's \
                      `<cwd>/.claude/skills/<name>/SKILL.md`. Embedded via \
                      `include_dir!` — a whole directory tree per skill, helper scripts \
                      included. Embedded-only today: the served-override half of plan \
                      2026-08-20-fleet-served-agent-skills is qontinui-web#1071, which \
                      has not landed, so a `served` reading on this row would itself be \
                      a finding.",
        expected_rungs: &[Rung::Embedded, Rung::Unresolved],
        anchor: "fleet_skills::provision_fleet_skills_into over FLEET_SKILLS / embedded_skill_count",
    },
    CapabilitySpec {
        id: "fleet_agents",
        class: "session_provisioning",
        description: "The named-subagent definitions written into a spawned session's \
                      `<cwd>/.claude/agents/*.md` — the definitions `claude` reads to \
                      resolve `code-reviewer`, `merge-specialist` and the rest. Embedded \
                      via `include_dir!` as a FLOOR beneath the checkout copy \
                      (`agent_definitions` below), which still wins where it exists. \
                      Without either, a spawned agent silently has no subagents: the \
                      named subagent does not resolve, the review never runs, and coord \
                      eventually ages the PR out as `specialist_timeout` — a failure \
                      with no error at the point of cause.",
        expected_rungs: &[Rung::Embedded, Rung::Unresolved],
        anchor: "fleet_agents (FLEET_AGENTS, include_dir!)",
    },
    CapabilitySpec {
        id: "agent_definitions",
        class: "session_provisioning",
        description: "The CHECKOUT copy of the same subagent definitions, read from \
                      `<workspace-root>/qontinui-claude-config/.claude/agents/*.md` off \
                      the operator's disk. It outranks the embedded floor, so on a box \
                      that has that sibling repo this row — not `fleet_agents` — decides \
                      what a session actually gets. Two sources for one asset, with \
                      nothing asserting they agree; when the root does not resolve the \
                      copy is a no-op that logs \"no qontinui-root resolved; skipping \
                      .claude/agents\" and continues.",
        expected_rungs: &[Rung::OperatorCheckout, Rung::Unresolved],
        anchor: "agent_runtime::provision_agent_definitions_from_root",
    },
    CapabilitySpec {
        id: "agent_commands_registry",
        class: "served_registry",
        description: "The account-versioned override registry for the agent command \
                      procedures: fetch `GET {base}/api/v1/agent-commands`, else the \
                      on-disk `agent-commands-cache.json`, else the embedded default. \
                      The one surface here that is genuinely operator-safe by design — \
                      and, until this manifest, the one where NOTHING reported which of \
                      the three arms won. The shape precedent for fixing that is \
                      `/health`'s `database.arm`, which already publishes exactly this \
                      kind of verdict.",
        expected_rungs: &[Rung::Served, Rung::DiskCache, Rung::Embedded],
        anchor: "agent_commands::resolve_registry (fetch_overrides_blocking / read_cache_at / builtin)",
    },
    CapabilitySpec {
        id: "slash_commands",
        class: "session_provisioning",
        description: "Import of `<workspace-root>/qontinui-claude-config/.claude/commands/*.md` \
                      as runner workflows. Purely a sibling-checkout scan with no \
                      embedded or bundled fallback of any kind, so on any device without \
                      that repo it returns `Err` and the workflows simply do not exist. \
                      The clearest instance of the class in the roster: it is not that \
                      this degrades on a published install, it is that it cannot run at \
                      all there.",
        expected_rungs: &[Rung::OperatorCheckout, Rung::Unresolved],
        anchor: "slash_commands::{find_commands_directory_reported, sync_slash_commands}",
    },
];

/// Look up a [`CapabilitySpec`] by id. Panics on an unknown id — every caller
/// passes a literal that IS in [`CAPABILITY_SPECS`], and a miss is a programming
/// error that must fail loudly rather than silently drop a capability.
#[must_use]
pub fn capability(id: &str) -> &'static CapabilitySpec {
    CAPABILITY_SPECS
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("no CAPABILITY_SPECS entry for capability {id:?}"))
}

// ===========================================================================
// The observation — what a resolver reported, handed in as data.
// ===========================================================================

/// One resolver's report about one capability, injected into
/// [`build_manifest`] by whoever can actually call that resolver.
///
/// This is the [`qontinui_runner_lib::coord_doctor::DoctorInputs`] /
/// `ConfigReportInputs` pattern: the driver owns the roster and the rendering,
/// the caller owns the observing, and an absent observation becomes an honest
/// [`Rung::Unknown`] rather than a silently-dropped row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilityObservation {
    /// Which rung answered.
    pub rung: Rung,
    /// The highest-priority candidate that was present but UNUSABLE — carried
    /// **even when [`rung`](Self::rung) is a successful resolution**. See the
    /// module header: this field is the only thing that separates "the dev
    /// checkout answered" from "the bundle was there, was rejected, and then the
    /// dev checkout answered", and those are different findings.
    pub rejected: Option<String>,
    /// The path that answered, when the capability resolves to one. Machine-
    /// local and therefore NOT comparable across boxes — Phase 5 diffs
    /// [`rung`](Self::rung), which is. Present so a reader can act on the row.
    pub resolved_path: Option<String>,
    /// Resolver-specific extra, in the resolver's own vocabulary — e.g. the
    /// upstream `WorkspaceRootKind::wire()` that a collapsed conversion would
    /// otherwise lose, or a count of files written.
    pub detail: Option<String>,
    /// Free prose for a reader. Also where a conversion's caveat lands (see
    /// [`Rung::from_command_source`]).
    pub note: Option<String>,
}

impl CapabilityObservation {
    /// A plain observation: this rung answered, nothing was rejected, no path.
    #[must_use]
    pub fn new(rung: Rung) -> Self {
        CapabilityObservation {
            rung,
            rejected: None,
            resolved_path: None,
            detail: None,
            note: None,
        }
    }

    /// Attach the rejected higher-priority candidate. Call this even on success.
    #[must_use]
    pub fn with_rejected(mut self, rejected: impl Into<String>) -> Self {
        self.rejected = Some(rejected.into());
        self
    }

    /// Attach the path that answered.
    #[must_use]
    pub fn with_resolved_path(mut self, path: impl Into<String>) -> Self {
        self.resolved_path = Some(path.into());
        self
    }

    /// Attach resolver-specific detail, in the resolver's own vocabulary.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Attach a note for the reader.
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Build an observation from a `qontinui_types::paths::WorkspaceRoot`,
    /// carrying **all three** of its fields across: the kind becomes the
    /// [`Rung`], the rejected candidate becomes
    /// [`rejected`](Self::rejected) — *even on success*, which is the whole
    /// reason `WorkspaceRoot` reports it — and the root becomes
    /// [`resolved_path`](Self::resolved_path).
    ///
    /// The upstream `kind.wire()` is preserved in
    /// [`detail`](Self::detail) so the `Declared`/`HomeDefault` collapse
    /// documented on [`impl From<WorkspaceRootKind> for Rung`](Rung) is
    /// recoverable from the manifest rather than lost in it.
    #[must_use]
    pub fn from_workspace_root_kind(
        kind: WorkspaceRootKind,
        root: Option<String>,
        rejected: Option<String>,
    ) -> Self {
        CapabilityObservation {
            rung: Rung::from(kind),
            rejected,
            resolved_path: root,
            detail: Some(format!("WorkspaceRootKind::{}", kind.wire())),
            note: None,
        }
    }

    /// Build an observation from an [`crate::agent_commands::CommandSource`].
    ///
    /// [`Rung::from_command_source`] no longer produces a caveat (Phase 3 split
    /// the upstream type instead of guessing), but the note slot is still
    /// threaded through so a future ambiguity cannot be introduced without a
    /// place to state it.
    #[must_use]
    pub fn from_command_source(source: CommandSource) -> Self {
        let (rung, note) = Rung::from_command_source(source);
        CapabilityObservation {
            rung,
            rejected: None,
            resolved_path: None,
            detail: Some(format!("CommandSource::{}", source.as_str())),
            note: note.map(str::to_string),
        }
    }
}

// ===========================================================================
// The provisioning ledger — Phase 3.
//
// Every `provision_*` path in this binary is fail-soft by contract: a
// provisioning failure must never abort an otherwise-launchable spawn. That
// posture is CORRECT and is unchanged by anything below. What was wrong is that
// the degradation existed only as a `warn!` — a line in a log file an external
// operator will never read — so a session that got six of seven commands, or
// zero subagent definitions, looked from outside exactly like one that got
// everything. These types make the degradation a VALUE the manifest can carry.
// ===========================================================================

/// Why one unit of a provisioning pass was not written.
///
/// A "unit" is whatever that provisioner writes one of: a command file, a skill
/// file, a subagent definition, or — where the whole pass could not run — the
/// destination directory itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum SkipReason {
    /// The destination exists and is TRACKED by the enclosing git repository,
    /// so writing it would replace that repo's own content and dirty its tree.
    /// The intended outcome, not a fault — see [`crate::provision_guard`] — but
    /// it still means the session did not get this unit.
    GitTracked,
    /// The write itself failed. Fail-soft: the pass continues and the spawn
    /// proceeds; the session simply lacks this unit.
    WriteFailed(String),
    /// The rung that would have supplied this unit did not resolve at all — no
    /// workspace root, no sibling checkout, no source directory. The
    /// checkout-bound case a published install hits by default.
    Unresolved(String),
}

impl SkipReason {
    /// Stable snake_case wire string, matching the `serde` tag.
    #[must_use]
    pub fn wire(&self) -> &'static str {
        match self {
            SkipReason::GitTracked => "git_tracked",
            SkipReason::WriteFailed(_) => "write_failed",
            SkipReason::Unresolved(_) => "unresolved",
        }
    }

    /// One line naming the reason, for the log summary and the manifest note.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            SkipReason::GitTracked => {
                "tracked by the enclosing git repository — left alone deliberately".to_string()
            }
            SkipReason::WriteFailed(why) => format!("write failed: {why}"),
            SkipReason::Unresolved(why) => format!("source rung did not resolve: {why}"),
        }
    }
}

/// One unit a provisioning pass did NOT write, and why.
///
/// The pair is the whole point: a count of skips with no reasons is the same
/// unreadable signal as the `warn!` it replaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedUnit {
    /// The unit's name as the provisioner addresses it — a file name, a
    /// relative path, or the destination directory.
    pub unit: String,
    /// Why it was not written.
    #[serde(flatten)]
    pub reason: SkipReason,
}

/// What one `provision_*` pass actually delivered.
///
/// **This type does not change fail-soft behaviour and must never be made to.**
/// It is a return value, not a gate: every caller in this binary logs it and
/// continues. Its job is to let the degradation be READ — by the capability
/// manifest, by a log line that states counts instead of a bare "failed", and
/// by a test that can assert a degraded pass reported its skipped units with
/// reasons rather than silently succeeding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProvisionReport {
    /// The [`CapabilitySpec::id`] this pass provisions. Ties the report to a
    /// manifest row without re-deriving anything.
    pub capability: &'static str,
    /// How many units this pass SHOULD have written, from the provisioner's own
    /// roster (`FLEET_COMMANDS.len()`, the embedded file count, …). A pass that
    /// wrote everything has `written == expected` and no skips.
    pub expected: usize,
    /// How many units were actually written.
    pub written: usize,
    /// Every unit that was not written, WITH its reason.
    pub skipped: Vec<SkippedUnit>,
    /// Which rung supplied what was written. [`Rung::Unresolved`] when the pass
    /// delivered nothing at all.
    pub rung: Rung,
    /// Where the pass wrote (or would have written). Machine-local, so not the
    /// field to diff across boxes.
    pub destination: Option<String>,
    /// Provisioner-specific extra, in its own vocabulary.
    pub detail: Option<String>,
}

impl ProvisionReport {
    /// An empty report for `capability`, expecting `expected` units on `rung`.
    #[must_use]
    pub fn new(capability: &'static str, expected: usize, rung: Rung) -> Self {
        ProvisionReport {
            capability,
            expected,
            written: 0,
            skipped: Vec::new(),
            rung,
            destination: None,
            detail: None,
        }
    }

    /// A report for a pass that could not run at all: nothing written, one
    /// skipped unit naming the destination and the reason, rung
    /// [`Rung::Unresolved`].
    ///
    /// This is the shape that replaces a lone `warn!` on a resolver that found
    /// no source — `agent_runtime`'s "no qontinui-root resolved; skipping
    /// .claude/agents" and `slash_commands`' missing-checkout `Err`.
    #[must_use]
    pub fn unresolved(
        capability: &'static str,
        expected: usize,
        unit: impl Into<String>,
        why: impl Into<String>,
    ) -> Self {
        let mut report = ProvisionReport::new(capability, expected, Rung::Unresolved);
        report.skip(unit, SkipReason::Unresolved(why.into()));
        report
    }

    /// Record where this pass wrote.
    #[must_use]
    pub fn with_destination(mut self, destination: impl Into<String>) -> Self {
        self.destination = Some(destination.into());
        self
    }

    /// Record provisioner-specific detail.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Count one written unit.
    pub fn record_written(&mut self) {
        self.written += 1;
    }

    /// Record one unit that was not written, with its reason.
    pub fn skip(&mut self, unit: impl Into<String>, reason: SkipReason) {
        self.skipped.push(SkippedUnit {
            unit: unit.into(),
            reason,
        });
    }

    /// Pin the rung after the fact — used where the rung is only knowable once
    /// the pass has run (nothing written at all is [`Rung::Unresolved`]).
    pub fn set_rung(&mut self, rung: Rung) {
        self.rung = rung;
    }

    /// True when every expected unit was written and nothing was skipped.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.skipped.is_empty() && self.written == self.expected
    }

    /// True when this pass delivered less than its roster — the state that used
    /// to be invisible.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        !self.is_complete()
    }

    /// One log line: counts first, then every skipped unit with its reason.
    ///
    /// Deliberately states the reasons rather than a count of them — a "3
    /// skipped" line is the same unreadable signal this type replaces.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = format!(
            "{}: wrote {}/{} unit(s) via `{}`",
            self.capability,
            self.written,
            self.expected,
            self.rung.wire()
        );
        if let Some(dest) = &self.destination {
            out.push_str(&format!(" into {dest}"));
        }
        if let Some(detail) = &self.detail {
            out.push_str(&format!(" ({detail})"));
        }
        if self.skipped.is_empty() {
            return out;
        }
        out.push_str(&format!("; {} skipped:", self.skipped.len()));
        for skip in &self.skipped {
            out.push_str(&format!(" [{} — {}]", skip.unit, skip.reason.describe()));
        }
        out
    }

    /// Turn the report into the manifest's observation for its capability.
    ///
    /// The skipped units land in [`CapabilityObservation::note`] WITH their
    /// reasons, so a manifest read on an operator's machine states what the
    /// session did not get and why — which is the entire deliverable of this
    /// phase.
    #[must_use]
    pub fn observation(&self) -> CapabilityObservation {
        let mut detail = format!("wrote {}/{} unit(s)", self.written, self.expected);
        if let Some(extra) = &self.detail {
            detail.push_str(&format!("; {extra}"));
        }
        let mut obs = CapabilityObservation::new(self.rung).with_detail(detail);
        if let Some(dest) = &self.destination {
            obs = obs.with_resolved_path(dest.clone());
        }
        if !self.skipped.is_empty() {
            let reasons = self
                .skipped
                .iter()
                .map(|s| format!("{} ({})", s.unit, s.reason.describe()))
                .collect::<Vec<_>>()
                .join("; ");
            obs = obs.with_note(format!(
                "{} unit(s) not provisioned — the spawn continued regardless (fail-soft \
                 by contract): {reasons}",
                self.skipped.len()
            ));
        }
        obs
    }
}

/// Every [`ProvisionReport`] one session's spawn produced, keyed by the working
/// directory that was provisioned.
///
/// A session's provisioning is several independent passes (commands, skills,
/// subagent definitions, the override registry that feeds the first of them),
/// each fail-soft on its own. Individually they are `warn!`s; together they are
/// the answer to "what did this session actually get?", which is why they are
/// collected per session rather than only per capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionProvisionLedger {
    /// The provisioned working directory.
    pub workdir: String,
    /// The passes, in the order they ran.
    pub reports: Vec<ProvisionReport>,
}

impl SessionProvisionLedger {
    /// The reports for passes that delivered less than their roster.
    #[must_use]
    pub fn degraded(&self) -> Vec<&ProvisionReport> {
        self.reports.iter().filter(|r| r.is_degraded()).collect()
    }

    /// One line per pass, for a log or an operator report.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("session provisioning ledger for {}\n", self.workdir);
        for report in &self.reports {
            out.push_str(&format!("  {}\n", report.summary()));
        }
        out
    }
}

/// How many sessions' ledgers are retained. A long-lived runner spawns
/// thousands of sessions; this store is a diagnostic, not a record, so it is
/// bounded and drops the oldest rather than growing without limit.
const LEDGER_CAPACITY: usize = 64;

/// The process-wide store. Two indexes over the same writes: per-session
/// ledgers (bounded, ordered) and the latest observation per capability (which
/// is what [`observed_inputs`] feeds into [`build_manifest`]).
struct ProvisionStore {
    sessions: std::collections::VecDeque<SessionProvisionLedger>,
    latest: std::collections::HashMap<&'static str, CapabilityObservation>,
}

fn store() -> &'static std::sync::Mutex<ProvisionStore> {
    static STORE: std::sync::OnceLock<std::sync::Mutex<ProvisionStore>> =
        std::sync::OnceLock::new();
    STORE.get_or_init(|| {
        std::sync::Mutex::new(ProvisionStore {
            sessions: std::collections::VecDeque::new(),
            latest: std::collections::HashMap::new(),
        })
    })
}

/// Take the store lock, recovering from a poisoned mutex rather than panicking.
///
/// A diagnostic that panics while reporting is worse than one that says it could
/// not — the same rule [`render_manifest_json`] applies to serialization. A
/// panic here would additionally reach a SPAWN path, which must never fail for a
/// reporting reason.
fn with_store<T>(f: impl FnOnce(&mut ProvisionStore) -> T) -> T {
    let mutex = store();
    let mut guard = match mutex.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    f(&mut guard)
}

/// Record one provisioning pass against `workdir`, and make its observation the
/// latest reading for that capability.
///
/// **Never fails and never panics**, because it is called from spawn paths whose
/// contract is that provisioning cannot abort a launch. It logs the report at
/// `info!` when the pass was complete and at `warn!` when it degraded — the same
/// two levels the provisioners used before, now carrying counts and reasons
/// instead of a bare sentence.
pub fn record_provision(workdir: &str, report: ProvisionReport) {
    if report.is_degraded() {
        tracing::warn!("provisioning degraded — {}", report.summary());
    } else {
        tracing::info!("provisioned — {}", report.summary());
    }
    with_store(|s| {
        s.latest.insert(report.capability, report.observation());
        match s.sessions.iter_mut().find(|l| l.workdir == workdir) {
            Some(ledger) => ledger.reports.push(report),
            None => {
                if s.sessions.len() >= LEDGER_CAPACITY {
                    s.sessions.pop_front();
                }
                s.sessions.push_back(SessionProvisionLedger {
                    workdir: workdir.to_string(),
                    reports: vec![report],
                });
            }
        }
    });
}

/// The ledger for `workdir`, or `None` when this process has provisioned no such
/// session (or its ledger has aged out of [`LEDGER_CAPACITY`]).
///
/// `None` is UNKNOWN, never "that session got nothing" — the store is bounded
/// and process-local.
#[must_use]
pub fn session_provision_ledger(workdir: &str) -> Option<SessionProvisionLedger> {
    with_store(|s| s.sessions.iter().find(|l| l.workdir == workdir).cloned())
}

/// Every retained session ledger, oldest first.
#[must_use]
pub fn session_provision_ledgers() -> Vec<SessionProvisionLedger> {
    with_store(|s| s.sessions.iter().cloned().collect())
}

/// The most recent observation recorded for `capability`, if any.
#[must_use]
pub fn latest_observation(capability: &str) -> Option<CapabilityObservation> {
    with_store(|s| s.latest.get(capability).cloned())
}

/// Record an observation for a capability that is not a provisioning pass —
/// a resolver that reports which rung answered without writing any units.
pub fn record_observation(capability: &'static str, observation: CapabilityObservation) {
    with_store(|s| {
        s.latest.insert(capability, observation);
    });
}

/// Discard everything recorded so far. Test support only: the store is
/// process-wide, and a test asserting on it must not see another test's writes.
#[cfg(test)]
pub(crate) fn reset_provision_store() {
    with_store(|s| {
        s.sessions.clear();
        s.latest.clear();
    });
}

/// The build-identity half plus one optional observation per capability.
///
/// Deliberately NOT `Default`-derived, for the reason `ConfigReportInputs`
/// gives: adding a capability must force every caller to decide whether it can
/// observe it, rather than silently defaulting to "no". Use
/// [`ManifestInputs::for_this_build`] to get the honest all-unobserved baseline
/// and then fill in what you can.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestInputs {
    /// 12-char git SHA of the commit this binary was built from.
    pub git_sha: String,
    /// Compile-time build provenance — which frontend bundle this binary
    /// embedded.
    pub build_id: String,
    /// The crate version, i.e. the released runner version.
    pub app_version: String,
    /// `workspace_root` — observed by [`crate::workspace_paths`].
    pub workspace_root: Option<CapabilityObservation>,
    /// `bundled_resources` — observed by [`crate::bundled_resources`].
    pub bundled_resources: Option<CapabilityObservation>,
    /// `spec_pages` — observed by [`crate::spec_api`]'s storage layer.
    pub spec_pages: Option<CapabilityObservation>,
    /// `fleet_commands` — observed by [`crate::fleet_commands`]'s provisioning.
    pub fleet_commands: Option<CapabilityObservation>,
    /// `fleet_skills` — observed by [`crate::fleet_skills`]'s provisioning.
    pub fleet_skills: Option<CapabilityObservation>,
    /// `fleet_agents` — observed by [`crate::fleet_agents`].
    pub fleet_agents: Option<CapabilityObservation>,
    /// `agent_definitions` — observed by [`crate::agent_runtime`].
    pub agent_definitions: Option<CapabilityObservation>,
    /// `agent_commands_registry` — observed by [`crate::agent_commands`].
    pub agent_commands_registry: Option<CapabilityObservation>,
    /// `slash_commands` — observed by [`crate::slash_commands`].
    pub slash_commands: Option<CapabilityObservation>,
}

impl ManifestInputs {
    /// This binary's identity plus **no observations at all** — the honest
    /// Phase 1 baseline, and the starting point every later phase fills in.
    ///
    /// The three identity values are the same compile-time constants
    /// `GET :9876/health` already publishes (`gitSha`, `buildId`), plus the
    /// crate version, so a manifest and a `/health` response taken from the same
    /// process are joinable.
    #[must_use]
    pub fn for_this_build() -> Self {
        ManifestInputs {
            git_sha: env!("QONTINUI_GIT_SHA").to_string(),
            build_id: env!("RUNNER_BUILD_ID").to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            workspace_root: None,
            bundled_resources: None,
            spec_pages: None,
            fleet_commands: None,
            fleet_skills: None,
            fleet_agents: None,
            agent_definitions: None,
            agent_commands_registry: None,
            slash_commands: None,
        }
    }

    /// This binary's identity plus every observation the process has recorded
    /// so far — the door Phase 3's ledger opens onto [`build_manifest`].
    ///
    /// A capability nothing has recorded stays `None` and therefore renders as
    /// [`Rung::Unknown`] naming its anchor, which is discipline (1) of this
    /// module: **an unobserved row is never filled in with the value the code
    /// would have fallen back to.** In particular, a runner that has spawned no
    /// session has provisioned nothing, so its session-provisioning rows are
    /// `unknown` rather than a guess at what a spawn would have produced.
    #[must_use]
    pub fn observed() -> Self {
        let mut inputs = ManifestInputs::for_this_build();
        inputs.workspace_root = latest_observation("workspace_root");
        inputs.bundled_resources = latest_observation("bundled_resources");
        inputs.spec_pages = latest_observation("spec_pages");
        inputs.fleet_commands = latest_observation("fleet_commands");
        inputs.fleet_skills = latest_observation("fleet_skills");
        inputs.fleet_agents = latest_observation("fleet_agents");
        inputs.agent_definitions = latest_observation("agent_definitions");
        inputs.agent_commands_registry = latest_observation("agent_commands_registry");
        inputs.slash_commands = latest_observation("slash_commands");
        inputs
    }

    /// **The Phase 4 driver.** Everything [`observed`](Self::observed) has, plus
    /// the two rows a caller can probe DIRECTLY, right here, read-only.
    ///
    /// Both doors — the pre-GUI `--capability-manifest` flag and
    /// `GET /capability-manifest` on a running instance — call this, so the two
    /// differ only in what the process has actually done, never in how the
    /// manifest is assembled.
    ///
    /// # Which rows are probed here, and which cannot be
    ///
    /// - `workspace_root` — probed. [`crate::workspace_paths::workspace_root_observation`]
    ///   resolves through the **read-only** settings door, so asking the
    ///   question cannot mint a `local_user_id`, run the `claude-accounts.json`
    ///   migration or rewrite the operator's `settings.json`. A diagnostic that
    ///   mutates state as a side effect of reporting has changed the answer by
    ///   asking it.
    /// - `bundled_resources` — probed. [`crate::bundled_resources::bundled_resources_observation`]
    ///   walks every runtime-resolved asset. From a pre-GUI caller it returns
    ///   [`Rung::Unknown`] rather than a checkout rung, because the bundle rung
    ///   needs a Tauri `AppHandle` that does not exist yet; see that function's
    ///   docs for why promoting the checkout reading would fabricate a finding.
    /// - `spec_pages` — **not probeable**. Its `root` comes from
    ///   `spec_api::storage::resolve_specs_root`, an async lookup in the `apps`
    ///   registry in Postgres. The arm is RECORDED as real reads take it, so a
    ///   process that has served none reports `unknown` naming its anchor —
    ///   never a guess at which arm a read would have taken.
    /// - the six provisioning / registry rows — **not probeable**. They are
    ///   facts about what a session SPAWN wrote, and a cold `--capability-manifest`
    ///   run has spawned nothing. `unknown` there is correct and is not a gap to
    ///   be filled: running a provisioning pass to observe one would write files
    ///   into somebody's worktree as a side effect of a report.
    ///
    /// The two probes overwrite whatever the ledger held for those rows on
    /// purpose: they are live readings taken now, while a ledger entry is a
    /// recording of something that happened earlier in this process.
    #[must_use]
    pub fn observed_here() -> Self {
        let mut inputs = ManifestInputs::observed();
        inputs.workspace_root = Some(crate::workspace_paths::workspace_root_observation());
        inputs.bundled_resources = Some(crate::bundled_resources::bundled_resources_observation());
        inputs
    }

    /// The observation this caller supplied for `id`, if any.
    ///
    /// A `match` rather than a map so a new [`CapabilitySpec`] row without a
    /// field here is caught by `every_capability_has_an_input_field` instead of
    /// silently reading `None` forever.
    fn observation(&self, id: &str) -> Option<&CapabilityObservation> {
        match id {
            "workspace_root" => self.workspace_root.as_ref(),
            "bundled_resources" => self.bundled_resources.as_ref(),
            "spec_pages" => self.spec_pages.as_ref(),
            "fleet_commands" => self.fleet_commands.as_ref(),
            "fleet_skills" => self.fleet_skills.as_ref(),
            "fleet_agents" => self.fleet_agents.as_ref(),
            "agent_definitions" => self.agent_definitions.as_ref(),
            "agent_commands_registry" => self.agent_commands_registry.as_ref(),
            "slash_commands" => self.slash_commands.as_ref(),
            _ => None,
        }
    }

    /// True iff this id has a field on this struct at all. Distinguishes "the
    /// caller did not observe it" from "this struct has nowhere to put it",
    /// which is a wiring bug rather than an observation gap.
    fn has_field_for(id: &str) -> bool {
        matches!(
            id,
            "workspace_root"
                | "bundled_resources"
                | "spec_pages"
                | "fleet_commands"
                | "fleet_skills"
                | "fleet_agents"
                | "agent_definitions"
                | "agent_commands_registry"
                | "slash_commands"
        )
    }
}

// ===========================================================================
// The manifest + the byte-stable renderers.
// ===========================================================================

/// One capability's row: what it is, and which rung answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilityRow {
    /// Matches the [`CapabilitySpec::id`] this row was built from.
    pub id: String,
    /// Which rung answered — [`Rung::Unknown`] when nothing observed this row.
    pub rung: Rung,
    /// The higher-priority candidate that was present but unusable. Carried even
    /// on success; see [`CapabilityObservation::rejected`].
    pub rejected: Option<String>,
    /// The path that answered. Machine-local, so not the field to diff.
    pub resolved_path: Option<String>,
    /// Resolver-specific extra in the resolver's own vocabulary.
    pub detail: Option<String>,
    /// Prose for the reader. For an [`Rung::Unknown`] row this is **never
    /// empty**: it names the symbol that owns the resolution, which is
    /// discipline (1) of this module.
    pub note: Option<String>,
}

/// The whole manifest: one row per [`CapabilitySpec`], plus the identity of the
/// binary that produced it.
///
/// Field order is the JSON key order and is part of the wire contract — a
/// reordering is a [`SCHEMA_VERSION`] bump.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilityManifest {
    /// See [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// 12-char git SHA of the commit this binary was built from.
    pub git_sha: String,
    /// Which frontend bundle this binary embedded.
    pub build_id: String,
    /// The runner version.
    pub app_version: String,
    /// One row per [`CAPABILITY_SPECS`] entry, in table order. Never fewer.
    pub rows: Vec<CapabilityRow>,
    /// How many rows did NOT resolve — [`Rung::Unresolved`] plus
    /// [`Rung::Unknown`].
    ///
    /// The two are counted together and rendered apart on purpose. As a NUMBER
    /// the honest statement is "this many capabilities this manifest cannot
    /// vouch for", and an unobserved row is exactly as un-vouched-for as a
    /// missed one. As a ROW they are opposite findings — one about the machine,
    /// one about this module — so the text render breaks the count back out and
    /// never lets a reader mistake the second for the first.
    pub unresolved_count: usize,
}

impl CapabilityManifest {
    /// The row for `id`, or `None` if this manifest has no such capability.
    #[must_use]
    pub fn row(&self, id: &str) -> Option<&CapabilityRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    /// How many rows are on this rung.
    #[must_use]
    pub fn count_on(&self, rung: Rung) -> usize {
        self.rows.iter().filter(|r| r.rung == rung).count()
    }

    /// Render the copy-pasteable text report — the `--capability-manifest` door's
    /// human form. Ordering, labels and field order are the contract: two runs on
    /// two machines must differ only where the capabilities differ.
    ///
    /// There is no branch that prints a resolution for an [`Rung::Unknown`] row,
    /// because such a row carries none: `resolved_path` is whatever the observer
    /// supplied, and an observer that supplied nothing is why the rung is
    /// `Unknown` in the first place.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("qontinui capability manifest — which rung answered, per capability\n");
        out.push_str("==================================================================\n");
        out.push_str(&format!("schema_version: {}\n", self.schema_version));
        out.push_str(&format!("git_sha:        {}\n", self.git_sha));
        out.push_str(&format!("build_id:       {}\n", self.build_id));
        out.push_str(&format!("app_version:    {}\n", self.app_version));
        out.push('\n');

        for (i, row) in self.rows.iter().enumerate() {
            let spec = capability(&row.id);
            out.push_str(&format!(
                "{:>2}. [{:<width$}] {} ({})\n",
                i + 1,
                row.rung.wire(),
                row.id,
                spec.class,
                width = RUNG_WIDTH,
            ));
            if let Some(path) = &row.resolved_path {
                out.push_str(&format!("      resolved_path: {path}\n"));
            }
            // Printed on its own line, and printed on SUCCESS too — a
            // fall-through that is not shown is a fall-through nobody knows
            // happened.
            if let Some(rejected) = &row.rejected {
                out.push_str(&format!("      rejected:      {rejected}\n"));
            }
            if let Some(detail) = &row.detail {
                out.push_str(&format!("      detail:        {detail}\n"));
            }
            if let Some(note) = &row.note {
                out.push_str(&format!("      note:          {note}\n"));
            }
            out.push_str(&format!("      expected:      {}\n", expected(spec)));
        }

        let unknown = self.count_on(Rung::Unknown);
        let unresolved = self.count_on(Rung::Unresolved);
        out.push_str("------------------------------------------------------------------\n");
        out.push_str(&format!(
            "{} capabilities — {} resolved, {} unresolved, {} unknown ({} not vouched for).\n",
            self.rows.len(),
            self.rows.len() - self.unresolved_count,
            unresolved,
            unknown,
            self.unresolved_count,
        ));
        // Say it in the OUTPUT, not only in the source: the manifest is only
        // useful if a reader trusts these two words to mean different things.
        out.push_str(
            "`unresolved` means every rung was tried and none answered — a finding about \
             THIS MACHINE.\n",
        );
        out.push_str(
            "`unknown` means nothing observed the capability here — a finding about THIS \
             BINARY's\nreporting, never about the machine. It is never a default value, and \
             a row is never\nomitted for being unobservable.\n",
        );
        out
    }
}

/// Column width for the rung label — `exe_relative_checkout` is the longest at
/// 21.
const RUNG_WIDTH: usize = 21;

/// The `expected_rungs` list as a comma-separated wire string, or an explicit
/// marker when a spec declares none.
fn expected(spec: &CapabilitySpec) -> String {
    if spec.expected_rungs.is_empty() {
        return "(none declared)".to_string();
    }
    spec.expected_rungs
        .iter()
        .map(|r| r.wire())
        .collect::<Vec<_>>()
        .join(", ")
}

// ===========================================================================
// The driver.
// ===========================================================================

/// Build the full manifest: every capability in [`CAPABILITY_SPECS`] order,
/// resolved from `inputs` where the caller could observe it and reported as
/// [`Rung::Unknown`] — **naming the owning symbol** — where it could not.
///
/// There is no path out of this function that produces fewer rows than
/// [`CAPABILITY_SPECS`] has entries. That is discipline (2) of this module, and
/// it is enforced by construction rather than by remembering.
#[must_use]
pub fn build_manifest(inputs: &ManifestInputs) -> CapabilityManifest {
    let rows: Vec<CapabilityRow> = CAPABILITY_SPECS
        .iter()
        .map(|spec| resolve_row(spec, inputs))
        .collect();
    let unresolved_count = rows.iter().filter(|r| !r.rung.is_resolved()).count();
    CapabilityManifest {
        schema_version: SCHEMA_VERSION,
        git_sha: inputs.git_sha.clone(),
        build_id: inputs.build_id.clone(),
        app_version: inputs.app_version.clone(),
        rows,
        unresolved_count,
    }
}

/// Resolve one capability into a row.
///
/// The two `Unknown` reasons are deliberately different sentences, because they
/// send a reader to different places: "no observation was injected" is a phase
/// that has not landed yet or a caller that could not look, while "this id has
/// no field on `ManifestInputs`" is a wiring bug in this module — the same split
/// `Observer::missing_injection_reason` makes in `config_report`.
fn resolve_row(spec: &'static CapabilitySpec, inputs: &ManifestInputs) -> CapabilityRow {
    match inputs.observation(spec.id) {
        Some(obs) => CapabilityRow {
            id: spec.id.to_string(),
            rung: obs.rung,
            rejected: obs.rejected.clone(),
            resolved_path: obs.resolved_path.clone(),
            detail: obs.detail.clone(),
            note: obs.note.clone(),
        },
        None if ManifestInputs::has_field_for(spec.id) => CapabilityRow {
            id: spec.id.to_string(),
            rung: Rung::Unknown,
            rejected: None,
            resolved_path: None,
            detail: None,
            note: Some(format!(
                "not observed here — no observation was injected for `{}`, which is \
                 resolved by `{}`. This is the ABSENCE of a reading, never a finding \
                 that the capability is missing.",
                spec.id, spec.anchor
            )),
        },
        None => CapabilityRow {
            id: spec.id.to_string(),
            rung: Rung::Unknown,
            rejected: None,
            resolved_path: None,
            detail: None,
            note: Some(format!(
                "capability `{}` (resolved by `{}`) has no field on `ManifestInputs`, \
                 so no caller can ever report it — this is a wiring bug in \
                 `capability_manifest`, not a machine finding.",
                spec.id, spec.anchor
            )),
        },
    }
}

/// Serialize a manifest as pretty JSON — the `--capability-manifest --json` door
/// and Phase 5's comparator input.
///
/// Pretty rather than compact on purpose: the output is checked into CI job
/// summaries and diffed by humans as well as by the script, and a one-line JSON
/// blob diffs as a single changed line no matter what changed inside it.
///
/// [`serde_json::to_string_pretty`] cannot fail for this type (no map with
/// non-string keys, no non-finite float, no failing custom `Serialize`), but the
/// error is reported rather than unwrapped: a diagnostic that panics while
/// reporting is worse than one that says it could not.
#[must_use]
pub fn render_manifest_json(manifest: &CapabilityManifest) -> String {
    match serde_json::to_string_pretty(manifest) {
        Ok(json) => json,
        Err(e) => serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "error": format!("capability manifest could not be serialized: {e}"),
        })
        .to_string(),
    }
}

// ===========================================================================
// Doc generator — the capability roster as markdown, from the same table.
//
// Same trick as `coord_doctor::render_onboarding_doc` and
// `config_report::render_layer_doc`: one source of truth for the roster means
// the doc cannot claim a capability the manifest does not have.
// ===========================================================================

/// Render the capability roster as markdown, generated entirely from
/// [`CAPABILITY_SPECS`] and [`Rung::ALL`]. Byte-stable, and a pure function of
/// those two tables — it reports no runtime value, so it is identical on every
/// machine for a given commit, which is what lets a freshness workflow gate it.
///
/// Phase 4 checks the output in at `qontinui-runner/docs/runner-capabilities.md`
/// and emits it via `qontinui-runner --capability-manifest-doc`.
#[must_use]
pub fn render_manifest_doc() -> String {
    let mut out = String::new();
    out.push_str("# Runner capabilities — which rung answers for each\n\n");
    out.push_str(
        "Several of the runner's assets are resolved through **rung-ordered resolvers \
         that try a developer checkout before falling back**. On the author's machine the \
         checkout rung answers; on an external operator's machine it does not, and the \
         fallback either silently differs or is absent. This document is the roster of \
         those capabilities; the running binary reports which rung actually answered for \
         each, and comparing a development build's report with a published build's report \
         is the parity check.\n\n",
    );
    out.push_str(
        "<!-- GENERATED — do not edit by hand. Regenerate FROM BASH (Git Bash on \
         Windows), from `src-tauri/`: `cargo run --quiet --bin qontinui-runner -- \
         --capability-manifest-doc > ../docs/runner-capabilities.md`; against an \
         installed binary the same flag works directly — `qontinui-runner \
         --capability-manifest-doc > docs/runner-capabilities.md`. NOT from PowerShell, \
         whose `>` writes UTF-16 or a BOM that `include_str!` cannot read at all. The \
         source of truth is `CAPABILITY_SPECS` in \
         `src-tauri/src/capability_manifest.rs`, and \
         `.github/workflows/capability-manifest-fresh.yml` fails any PR whose checked-in \
         copy differs from a fresh render. -->\n\n",
    );
    out.push_str(&format!("Manifest schema version: `{SCHEMA_VERSION}`.\n\n"));

    out.push_str("## The rungs\n\n");
    out.push_str(
        "Ordered from *carried by the build* down to *found on the operator's disk*, then \
         the two non-answers. A capability resolving near the top resolves the same way on \
         every machine; one resolving near the bottom resolves only where a checkout \
         happens to exist.\n\n",
    );
    for rung in Rung::ALL {
        out.push_str(&format!("- `{}` — {}\n", rung.wire(), rung.describe()));
    }
    out.push('\n');
    out.push_str(
        "`unresolved` and `unknown` are **not** synonyms. `unresolved` is a finding about \
         the machine: every rung was tried and none answered. `unknown` is a finding about \
         the reporting binary: nothing observed the capability there. A row is never \
         omitted for being unobservable, and an unobservable row is never rendered as the \
         value the code would have fallen back to.\n\n",
    );

    out.push_str("## The capabilities\n\n");
    for (i, spec) in CAPABILITY_SPECS.iter().enumerate() {
        out.push_str(&format!("### {}. `{}`\n\n", i + 1, spec.id));
        out.push_str(&format!("{}\n\n", spec.description));
        out.push_str(&format!("- Class: `{}`\n", spec.class));
        out.push_str(&format!("- Resolved by: `{}`\n", spec.anchor));
        out.push_str(&format!("- Expected rungs: {}\n\n", expected(spec)));
    }

    out.push_str("---\n\n");
    out.push_str(
        "This roster is incomplete by construction: a manifest reports only what someone \
         thought to list, so it cannot catch a capability nobody enumerated. That is why \
         the parity check has a second, behavioural axis, and why a disagreement between \
         the two is reported as a finding **about this roster** rather than reconciled \
         toward either side. Adding a capability is adding a row to `CAPABILITY_SPECS`.\n",
    );
    out
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn unobserved_inputs() -> ManifestInputs {
        ManifestInputs {
            git_sha: "0123456789ab".to_string(),
            build_id: "0123456789ab-1756600000000".to_string(),
            app_version: "1.0.10".to_string(),
            workspace_root: None,
            bundled_resources: None,
            spec_pages: None,
            fleet_commands: None,
            fleet_skills: None,
            fleet_agents: None,
            agent_definitions: None,
            agent_commands_registry: None,
            slash_commands: None,
        }
    }

    // ------------------------------------------------------------------
    // The rung vocabulary: total, stable, and distinct.
    // ------------------------------------------------------------------

    /// `Rung::ALL` must actually list every variant. The exhaustive match is the
    /// mechanism — adding a variant without adding it to `ALL` fails to compile
    /// here rather than silently shrinking the generated doc.
    #[test]
    fn rung_all_covers_every_variant() {
        for rung in Rung::ALL {
            // Exhaustive by construction: a new variant breaks this match.
            match rung {
                Rung::Embedded
                | Rung::BundleResource
                | Rung::Served
                | Rung::DiskCache
                | Rung::ExeRelativeCheckout
                | Rung::DevCheckout
                | Rung::OperatorCheckout
                | Rung::Unresolved
                | Rung::Unknown => {}
            }
        }
        assert_eq!(
            Rung::ALL.len(),
            9,
            "the rung vocabulary is nine rungs — seven real ones plus the two \
             non-answers, which are not interchangeable"
        );
    }

    /// The wire strings are asserted against LITERALS, in table order.
    /// Comparing the table to itself would pin nothing, and these strings are
    /// the diff key Phase 5's comparator joins on — a change here is a
    /// `SCHEMA_VERSION` bump, and this test is what makes that visible.
    #[test]
    fn rung_wire_strings_are_stable_literals() {
        let wires: Vec<&str> = Rung::ALL.iter().map(|r| r.wire()).collect();
        assert_eq!(
            wires,
            vec![
                "embedded",
                "bundle_resource",
                "served",
                "disk_cache",
                "exe_relative_checkout",
                "dev_checkout",
                "operator_checkout",
                "unresolved",
                "unknown",
            ]
        );
    }

    /// No two rungs may share a wire string: the comparator distinguishes builds
    /// by this value alone, so a collision would silently make two different
    /// resolutions compare equal.
    #[test]
    fn rung_wire_strings_are_distinct() {
        let unique: BTreeSet<&str> = Rung::ALL.iter().map(|r| r.wire()).collect();
        assert_eq!(
            unique.len(),
            Rung::ALL.len(),
            "rung wire strings must be distinct"
        );
    }

    /// Every rung must have a non-empty description, and `RUNG_WIDTH` must
    /// actually fit the longest wire string — a too-narrow column would ragged
    /// the text render and make it diff noisily for no information.
    #[test]
    fn rung_descriptions_and_column_width_hold() {
        for rung in Rung::ALL {
            assert!(
                !rung.describe().is_empty(),
                "rung {:?} has no description",
                rung
            );
            assert!(
                rung.wire().len() <= RUNG_WIDTH,
                "RUNG_WIDTH {} is too narrow for {:?} ({})",
                RUNG_WIDTH,
                rung,
                rung.wire().len()
            );
        }
        assert_eq!(
            Rung::ALL.iter().map(|r| r.wire().len()).max(),
            Some(RUNG_WIDTH),
            "RUNG_WIDTH must be exactly the longest wire string, not merely wide enough"
        );
    }

    /// The JSON encoding is the `wire()` string and nothing else. The two are
    /// one source of truth (`impl Serialize` delegates), and this pins it so a
    /// future `rename_all` attribute cannot quietly fork them.
    #[test]
    fn rung_serializes_as_its_wire_string() {
        for rung in Rung::ALL {
            let json = serde_json::to_string(rung).expect("Rung serializes");
            assert_eq!(json, format!("\"{}\"", rung.wire()));
        }
    }

    /// `is_resolved` splits the nine rungs into exactly seven real resolutions
    /// and the two non-answers, and `is_checkout_bound` picks out the three that
    /// are the parity class itself. Both are asserted as literal sets.
    #[test]
    fn rung_predicates_partition_the_vocabulary() {
        let resolved: Vec<&str> = Rung::ALL
            .iter()
            .filter(|r| r.is_resolved())
            .map(|r| r.wire())
            .collect();
        assert_eq!(
            resolved,
            vec![
                "embedded",
                "bundle_resource",
                "served",
                "disk_cache",
                "exe_relative_checkout",
                "dev_checkout",
                "operator_checkout",
            ]
        );

        let checkout_bound: Vec<&str> = Rung::ALL
            .iter()
            .filter(|r| r.is_checkout_bound())
            .map(|r| r.wire())
            .collect();
        assert_eq!(
            checkout_bound,
            vec!["exe_relative_checkout", "dev_checkout", "operator_checkout"]
        );
    }

    // ------------------------------------------------------------------
    // The conversions — a superset that existing vocabularies map INTO.
    // ------------------------------------------------------------------

    /// Every `WorkspaceRootKind` variant maps, and the mapping is asserted
    /// per-variant. The `match` in the test is exhaustive with no `_` arm on
    /// purpose: a variant added upstream must break this build, which is the
    /// entire mechanism keeping `Rung` a superset rather than a fifth parallel
    /// vocabulary.
    #[test]
    fn workspace_root_kind_maps_into_every_rung_it_should() {
        let all = [
            WorkspaceRootKind::Declared,
            WorkspaceRootKind::Discovered,
            WorkspaceRootKind::HomeDefault,
            WorkspaceRootKind::Unresolved,
        ];
        for kind in all {
            let expected = match kind {
                WorkspaceRootKind::Declared => Rung::OperatorCheckout,
                WorkspaceRootKind::Discovered => Rung::ExeRelativeCheckout,
                WorkspaceRootKind::HomeDefault => Rung::OperatorCheckout,
                WorkspaceRootKind::Unresolved => Rung::Unresolved,
            };
            assert_eq!(
                Rung::from(kind),
                expected,
                "WorkspaceRootKind::{} mapped wrong",
                kind.wire()
            );
        }
        // `Unresolved` upstream must stay `Unresolved` here and must never
        // become `Unknown`: the first is a finding about the machine, the second
        // about this module, and the whole manifest turns on the difference.
        assert_eq!(Rung::from(WorkspaceRootKind::Unresolved), Rung::Unresolved);
    }

    /// The `Declared`/`HomeDefault` collapse onto one rung is deliberate, and
    /// the upstream verdict must survive it in `detail` — otherwise the manifest
    /// would silently lose the distinction between a declared root and a
    /// fallen-through convention.
    #[test]
    fn workspace_root_observation_preserves_the_upstream_kind_and_rejection() {
        let obs = CapabilityObservation::from_workspace_root_kind(
            WorkspaceRootKind::HomeDefault,
            Some("/home/dev/qontinui-root".to_string()),
            Some("$QONTINUI_ROOT is not an existing directory".to_string()),
        );
        assert_eq!(obs.rung, Rung::OperatorCheckout);
        assert_eq!(
            obs.detail.as_deref(),
            Some("WorkspaceRootKind::home_default")
        );
        assert_eq!(
            obs.resolved_path.as_deref(),
            Some("/home/dev/qontinui-root")
        );
        // The load-bearing one: a rejection is carried EVEN THOUGH the
        // resolution succeeded.
        assert!(obs.rung.is_resolved());
        assert_eq!(
            obs.rejected.as_deref(),
            Some("$QONTINUI_ROOT is not an existing directory"),
            "`rejected` must survive a SUCCESSFUL resolution — it is what \
             distinguishes 'the fallback answered' from 'the fallback answered \
             because the bundle was broken'"
        );
    }

    /// Every `CommandSource` variant maps to a DISTINCT rung, with no caveat
    /// left over.
    ///
    /// This test used to be `command_source_maps_and_refuses_to_guess_the_account_arm`
    /// and asserted the opposite: that `Account → Unknown` carrying a caveat
    /// naming `resolve_registry`. That was the honest reading of a two-variant
    /// type against a three-arm resolver. Phase 3 split the type instead, so the
    /// mapping is exact and the caveat is gone — which is a stronger property
    /// than the one it replaces, not a relaxed one.
    #[test]
    fn every_command_source_maps_to_a_distinct_rung_with_no_caveat() {
        let all = [
            CommandSource::Builtin,
            CommandSource::Served,
            CommandSource::DiskCache,
        ];
        let mut rungs = Vec::new();
        for source in all {
            let (rung, note) = Rung::from_command_source(source);
            // Exhaustive, no `_` arm: a fourth variant upstream breaks this.
            match source {
                CommandSource::Builtin => assert_eq!(rung, Rung::Embedded),
                CommandSource::Served => assert_eq!(rung, Rung::Served),
                CommandSource::DiskCache => assert_eq!(rung, Rung::DiskCache),
            }
            assert!(
                note.is_none(),
                "an exact one-to-one mapping needs no caveat; {source:?} produced one"
            );
            // The bare `From` must agree with the noted form.
            assert_eq!(Rung::from(source), rung);
            rungs.push(rung);
        }
        let distinct: std::collections::HashSet<&'static str> =
            rungs.iter().map(|r| r.wire()).collect();
        assert_eq!(
            distinct.len(),
            all.len(),
            "two arms sharing a rung is the collapse Phase 3 removed"
        );
    }

    /// The observation builder states the upstream variant in `detail`, so the
    /// manifest carries the resolver's own vocabulary alongside the rung.
    #[test]
    fn command_source_observation_states_the_upstream_variant() {
        for (source, rung, wire) in [
            (CommandSource::Builtin, Rung::Embedded, "builtin"),
            (CommandSource::Served, Rung::Served, "served"),
            (CommandSource::DiskCache, Rung::DiskCache, "disk_cache"),
        ] {
            let obs = CapabilityObservation::from_command_source(source);
            assert_eq!(obs.rung, rung);
            assert_eq!(
                obs.detail.as_deref(),
                Some(&*format!("CommandSource::{wire}"))
            );
            assert!(
                obs.note.is_none(),
                "no arm is ambiguous any more, so no row should carry a caveat"
            );
        }
    }

    /// The skill-source mapping is now over a TYPE, and returns `Unknown` —
    /// never a guess — for the two variants whose rung this binary cannot state.
    #[test]
    fn skill_source_maps_the_typed_values_and_never_guesses() {
        assert_eq!(rung_for_skill_source(&SkillSource::Builtin), Rung::Embedded);
        assert_eq!(rung_for_skill_source(&SkillSource::User), Rung::DiskCache);
        // Phase 1 recorded "produced by no code path"; that is FALSE against the
        // real code (`mcp::skills::sync_pull` stamps it, `PgDb::import_skills`
        // persists it). It stays `Unknown` for a different and better reason:
        // the value straddles `served` (where the row was authored) and
        // `disk_cache` (where this binary reads it back), and one field cannot
        // state both.
        assert_eq!(
            rung_for_skill_source(&SkillSource::Community),
            Rung::Unknown
        );

        for unrecognised in ["", "Builtin", " builtin", "vendor", "embedded"] {
            let parsed = SkillSource::from_wire(unrecognised);
            assert_eq!(parsed, SkillSource::Other(unrecognised.to_string()));
            assert_eq!(
                rung_for_skill_source(&parsed),
                Rung::Unknown,
                "unrecognised skill source {unrecognised:?} must be Unknown, never guessed"
            );
        }
    }

    // ------------------------------------------------------------------
    // The provisioning ledger (Phase 3).
    // ------------------------------------------------------------------

    /// Serializes the two tests that mutate the PROCESS-WIDE provisioning
    /// store. `cargo test` runs a binary's tests in parallel threads, so two
    /// tests each calling `reset_provision_store` would otherwise erase each
    /// other's writes intermittently — a flake, not a finding.
    fn store_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// **The Phase 3 gate.** A degraded provisioning pass reports its skipped
    /// units WITH reasons instead of silently succeeding.
    ///
    /// The pass below wrote 5 of 7 units — the exact shape a session gets when
    /// its cwd is a checkout that tracks some of the destinations. Before this
    /// phase that fact existed only as two `info!` lines and an aggregate count,
    /// so from outside the process it was indistinguishable from a complete
    /// pass. Every assertion here is about it being a VALUE.
    #[test]
    fn a_degraded_pass_reports_its_skipped_units_with_reasons() {
        let mut report = ProvisionReport::new("fleet_commands", 7, Rung::Embedded)
            .with_destination("/tmp/wt/.claude/commands");
        for _ in 0..5 {
            report.record_written();
        }
        report.skip("vet-plan.md", SkipReason::GitTracked);
        report.skip(
            "gate.md",
            SkipReason::WriteFailed("permission denied".to_string()),
        );

        assert!(
            report.is_degraded(),
            "5 of 7 written is a degraded pass, not a successful one"
        );
        assert!(!report.is_complete());
        assert_eq!(report.skipped.len(), 2);

        // Every skip names its unit AND its reason — the pairing is the point.
        assert_eq!(report.skipped[0].unit, "vet-plan.md");
        assert_eq!(report.skipped[0].reason, SkipReason::GitTracked);
        assert_eq!(report.skipped[1].unit, "gate.md");
        assert_eq!(report.skipped[1].reason.wire(), "write_failed");

        // The log line states the reasons, not just how many there were.
        let summary = report.summary();
        assert!(summary.contains("wrote 5/7"), "{summary}");
        assert!(summary.contains("vet-plan.md"), "{summary}");
        assert!(summary.contains("permission denied"), "{summary}");

        // And the manifest row carries them out of the process.
        let obs = report.observation();
        assert_eq!(obs.rung, Rung::Embedded);
        assert_eq!(obs.detail.as_deref(), Some("wrote 5/7 unit(s)"));
        let note = obs.note.expect("a degraded pass must carry a note");
        assert!(note.contains("vet-plan.md"), "{note}");
        assert!(
            note.contains("tracked by the enclosing git repository"),
            "{note}"
        );
        assert!(note.contains("permission denied"), "{note}");
        // The fail-soft contract is stated in the row itself, so a reader does
        // not mistake a degraded pass for an aborted spawn.
        assert!(note.contains("the spawn continued regardless"), "{note}");
    }

    /// A COMPLETE pass carries no skip note — so the note's presence is itself
    /// the signal, and a clean manifest row is not merely an empty one.
    #[test]
    fn a_complete_pass_carries_no_skip_note() {
        let mut report = ProvisionReport::new("fleet_agents", 3, Rung::Embedded);
        for _ in 0..3 {
            report.record_written();
        }
        assert!(report.is_complete());
        let obs = report.observation();
        assert!(obs.note.is_none());
        assert_eq!(obs.detail.as_deref(), Some("wrote 3/3 unit(s)"));
    }

    /// The `unresolved` constructor is the shape that replaces a lone `warn!` on
    /// a resolver that found no source at all — `agent_runtime`'s "no
    /// qontinui-root resolved; skipping .claude/agents" and `slash_commands`'
    /// missing-checkout error.
    #[test]
    fn an_unresolved_pass_states_the_rung_and_the_reason() {
        let report = ProvisionReport::unresolved(
            "agent_definitions",
            0,
            "/wt/.claude/agents",
            "no qontinui-root resolved",
        );
        assert_eq!(report.rung, Rung::Unresolved);
        assert_eq!(report.written, 0);
        assert!(report.is_degraded());
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(
            report.skipped[0].reason,
            SkipReason::Unresolved("no qontinui-root resolved".to_string())
        );
        let obs = report.observation();
        assert_eq!(obs.rung, Rung::Unresolved);
        assert!(obs
            .note
            .is_some_and(|n| n.contains("no qontinui-root resolved")));
    }

    /// Skip reasons serialize as a tagged shape rather than as prose, so a
    /// comparator can group by reason without parsing English.
    #[test]
    fn skip_reasons_serialize_as_a_tagged_shape() {
        let unit = SkippedUnit {
            unit: "gate.md".to_string(),
            reason: SkipReason::WriteFailed("disk full".to_string()),
        };
        let json = serde_json::to_value(&unit).unwrap();
        assert_eq!(json["unit"], "gate.md");
        assert_eq!(json["reason"], "write_failed");
        assert_eq!(json["detail"], "disk full");

        let tracked = SkippedUnit {
            unit: "vet-plan.md".to_string(),
            reason: SkipReason::GitTracked,
        };
        let json = serde_json::to_value(&tracked).unwrap();
        assert_eq!(json["reason"], "git_tracked");
        assert!(json.get("detail").is_none(), "a unit variant carries none");
    }

    /// The per-session ledger collects one row per pass, and the manifest reads
    /// the latest observation back out — the whole path from a `provision_*`
    /// call to a manifest row, with no intermediate log file.
    ///
    /// Runs `#[serial]`-style by construction rather than by attribute: it is
    /// the only test that writes the process-wide store, and it resets it first.
    #[test]
    fn the_session_ledger_reaches_the_manifest() {
        let _guard = store_lock();
        reset_provision_store();
        let workdir = "/tmp/agent-worktree-ledger-test";

        let mut commands = ProvisionReport::new("fleet_commands", 7, Rung::Embedded)
            .with_destination(format!("{workdir}/.claude/commands"));
        for _ in 0..6 {
            commands.record_written();
        }
        commands.skip("policy.md", SkipReason::GitTracked);
        record_provision(workdir, commands);

        let mut skills = ProvisionReport::new("fleet_skills", 13, Rung::Embedded);
        for _ in 0..13 {
            skills.record_written();
        }
        record_provision(workdir, skills);

        record_provision(
            workdir,
            ProvisionReport::unresolved(
                "agent_definitions",
                0,
                format!("{workdir}/.claude/agents"),
                "no qontinui-root resolved",
            ),
        );

        let ledger = session_provision_ledger(workdir).expect("the ledger was recorded");
        assert_eq!(ledger.workdir, workdir);
        assert_eq!(ledger.reports.len(), 3, "one row per pass, in order");
        assert_eq!(
            ledger
                .reports
                .iter()
                .map(|r| r.capability)
                .collect::<Vec<_>>(),
            vec!["fleet_commands", "fleet_skills", "agent_definitions"]
        );

        // Two of the three degraded; the ledger says WHICH, not just how many.
        let degraded: Vec<&'static str> = ledger.degraded().iter().map(|r| r.capability).collect();
        assert_eq!(degraded, vec!["fleet_commands", "agent_definitions"]);

        let rendered = ledger.render();
        assert!(rendered.contains("wrote 6/7"), "{rendered}");
        assert!(rendered.contains("policy.md"), "{rendered}");
        assert!(rendered.contains("no qontinui-root resolved"), "{rendered}");

        // ...and the manifest picks it up with nothing else wired.
        let manifest = build_manifest(&ManifestInputs::observed());
        assert_eq!(manifest.row("fleet_commands").unwrap().rung, Rung::Embedded);
        assert_eq!(manifest.row("fleet_skills").unwrap().rung, Rung::Embedded);
        assert_eq!(
            manifest.row("agent_definitions").unwrap().rung,
            Rung::Unresolved
        );
        assert!(manifest
            .row("fleet_commands")
            .unwrap()
            .note
            .as_deref()
            .is_some_and(|n| n.contains("policy.md")));

        // A capability nothing recorded stays `unknown` NAMING ITS ANCHOR — an
        // unobserved row is never filled in with a plausible value.
        let unobserved = manifest.row("bundled_resources").unwrap();
        assert_eq!(unobserved.rung, Rung::Unknown);
        assert!(unobserved
            .note
            .as_deref()
            .is_some_and(|n| n.contains("bundled_resources::resolve_with_rung")));

        reset_provision_store();
    }

    /// The ledger is bounded: a long-lived runner spawns thousands of sessions,
    /// and this store is a diagnostic rather than a record.
    #[test]
    fn the_ledger_is_bounded_and_drops_the_oldest() {
        let _guard = store_lock();
        reset_provision_store();
        for i in 0..(LEDGER_CAPACITY + 5) {
            record_provision(
                &format!("/tmp/bounded-{i}"),
                ProvisionReport::new("fleet_skills", 0, Rung::Embedded),
            );
        }
        let ledgers = session_provision_ledgers();
        assert_eq!(ledgers.len(), LEDGER_CAPACITY);
        assert!(
            session_provision_ledger("/tmp/bounded-0").is_none(),
            "the oldest session must have been dropped"
        );
        assert!(
            session_provision_ledger(&format!("/tmp/bounded-{}", LEDGER_CAPACITY + 4)).is_some(),
            "the newest session must be retained"
        );
        reset_provision_store();
    }

    // ------------------------------------------------------------------
    // The roster.
    // ------------------------------------------------------------------

    /// The roster's ids are asserted against LITERALS. These are the join key
    /// for Phase 5's comparator and the anchors in the generated doc, so they
    /// are a contract; deriving the list from the table would pin nothing.
    #[test]
    fn capability_specs_are_the_seeded_roster_with_unique_ids() {
        let ids: Vec<&str> = CAPABILITY_SPECS.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec![
                "workspace_root",
                "bundled_resources",
                "spec_pages",
                "fleet_commands",
                "fleet_skills",
                "fleet_agents",
                "agent_definitions",
                "agent_commands_registry",
                "slash_commands",
            ]
        );
        let unique: BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "capability ids must be unique");
    }

    /// Every spec must carry real content: an empty description or a missing
    /// anchor would defeat the generated doc and, worse, the `Unknown` note that
    /// has to name the owning symbol.
    #[test]
    fn every_capability_spec_is_fully_populated() {
        let classes: BTreeSet<&str> = BTreeSet::from([
            "path_resolution",
            "bundled_asset",
            "session_provisioning",
            "served_registry",
        ]);
        for spec in CAPABILITY_SPECS {
            assert!(!spec.id.is_empty(), "empty capability id");
            assert!(
                classes.contains(spec.class),
                "capability {:?} has an unrecognised class {:?}",
                spec.id,
                spec.class
            );
            assert!(
                spec.description.len() > 80,
                "capability {:?} needs a real description, not a label",
                spec.id
            );
            assert!(
                !spec.anchor.is_empty(),
                "capability {:?} has no anchor — an unobserved row could not name \
                 its owning symbol",
                spec.id
            );
            assert!(
                !spec.expected_rungs.is_empty(),
                "capability {:?} declares no expected rungs",
                spec.id
            );
            assert!(
                !spec.expected_rungs.contains(&Rung::Unknown),
                "capability {:?} lists `unknown` as EXPECTED — `unknown` is a \
                 statement about the observer and can never be an expectation \
                 about a capability",
                spec.id
            );
        }
    }

    /// Every capability must have somewhere for a caller to put its observation.
    /// Without this, a row added to the table would report `Unknown` forever and
    /// the wiring bug would look exactly like an unlanded phase.
    #[test]
    fn every_capability_has_an_input_field() {
        for spec in CAPABILITY_SPECS {
            assert!(
                ManifestInputs::has_field_for(spec.id),
                "capability {:?} has no field on ManifestInputs, so no caller can \
                 ever report it",
                spec.id
            );
        }
    }

    // ------------------------------------------------------------------
    // The driver's honesty — discipline (1) and (2).
    // ------------------------------------------------------------------

    /// An unobservable row renders `Unknown`, **names the symbol that owns it**,
    /// and is NOT omitted. This is the module's central commitment and the one
    /// test that would catch its loss.
    #[test]
    fn unobserved_rows_render_unknown_naming_their_owning_symbol_and_are_never_omitted() {
        let manifest = build_manifest(&unobserved_inputs());

        assert_eq!(
            manifest.rows.len(),
            CAPABILITY_SPECS.len(),
            "every capability gets a row, observed or not"
        );

        for spec in CAPABILITY_SPECS {
            let row = manifest
                .row(spec.id)
                .unwrap_or_else(|| panic!("capability {:?} was omitted", spec.id));
            assert_eq!(
                row.rung,
                Rung::Unknown,
                "capability {:?} was not observed by anything in Phase 1, so its \
                 rung must be `unknown` — never the value the code would have \
                 fallen back to",
                spec.id
            );
            let note = row
                .note
                .as_deref()
                .unwrap_or_else(|| panic!("capability {:?} has no note", spec.id));
            assert!(
                note.contains(spec.anchor),
                "capability {:?}'s unknown note must name its owning symbol {:?}, \
                 got {:?}",
                spec.id,
                spec.anchor,
                note
            );
            // An unknown row carries no invented resolution.
            assert!(row.resolved_path.is_none());
            assert!(row.rejected.is_none());
            assert!(row.detail.is_none());
        }

        assert_eq!(
            manifest.unresolved_count,
            CAPABILITY_SPECS.len(),
            "nothing is observed in Phase 1, so nothing is vouched for"
        );
        assert_eq!(manifest.count_on(Rung::Unresolved), 0);
        assert_eq!(manifest.count_on(Rung::Unknown), CAPABILITY_SPECS.len());
    }

    /// An injected observation reaches the row intact — every field, including
    /// `rejected` on a SUCCESSFUL resolution.
    #[test]
    fn an_injected_observation_reaches_the_row_intact() {
        let mut inputs = unobserved_inputs();
        inputs.bundled_resources = Some(
            CapabilityObservation::new(Rung::DevCheckout)
                .with_resolved_path("/src/qontinui-runner/src-tauri/data/htn_methods")
                .with_rejected("bundle resource dir resolved but the file is absent")
                .with_detail("candidate index 2 of 3")
                .with_note("dev box"),
        );
        let manifest = build_manifest(&inputs);
        let row = manifest.row("bundled_resources").expect("row present");

        assert_eq!(row.rung, Rung::DevCheckout);
        assert_eq!(
            row.resolved_path.as_deref(),
            Some("/src/qontinui-runner/src-tauri/data/htn_methods")
        );
        assert_eq!(
            row.rejected.as_deref(),
            Some("bundle resource dir resolved but the file is absent")
        );
        assert_eq!(row.detail.as_deref(), Some("candidate index 2 of 3"));
        assert_eq!(row.note.as_deref(), Some("dev box"));

        // One row resolved; the rest are still unknown, and the count says so.
        assert_eq!(manifest.unresolved_count, CAPABILITY_SPECS.len() - 1);
    }

    /// `unresolved_count` counts BOTH non-answers, and the render breaks them
    /// back apart — the split the field's docs promise.
    #[test]
    fn unresolved_count_covers_both_non_answers_and_the_render_separates_them() {
        let mut inputs = unobserved_inputs();
        inputs.slash_commands = Some(CapabilityObservation::new(Rung::Unresolved));
        inputs.fleet_commands = Some(CapabilityObservation::new(Rung::Embedded));

        let manifest = build_manifest(&inputs);
        assert_eq!(manifest.count_on(Rung::Unresolved), 1);
        assert_eq!(manifest.count_on(Rung::Unknown), CAPABILITY_SPECS.len() - 2);
        assert_eq!(manifest.unresolved_count, CAPABILITY_SPECS.len() - 1);

        let text = manifest.render();
        assert!(text.contains("1 unresolved"));
        assert!(text.contains(&format!("{} unknown", CAPABILITY_SPECS.len() - 2)));
        assert!(text.contains("a finding about THIS MACHINE"));
        assert!(text.contains("never about the machine"));
    }

    /// The identity half is carried through verbatim, and `schema_version` ships
    /// from this first commit — the comparator needs it before the format ever
    /// changes, not after.
    #[test]
    fn manifest_carries_build_identity_and_schema_version() {
        let manifest = build_manifest(&unobserved_inputs());
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.schema_version, SCHEMA_VERSION);
        assert_eq!(manifest.git_sha, "0123456789ab");
        assert_eq!(manifest.build_id, "0123456789ab-1756600000000");
        assert_eq!(manifest.app_version, "1.0.10");
    }

    /// `for_this_build` reports THIS binary rather than a placeholder, and
    /// observes nothing — the honest Phase 1 baseline.
    #[test]
    fn for_this_build_reports_this_binary_and_observes_nothing() {
        let inputs = ManifestInputs::for_this_build();
        assert_eq!(inputs.app_version, env!("CARGO_PKG_VERSION"));
        assert!(!inputs.git_sha.is_empty());
        assert!(!inputs.build_id.is_empty());
        let manifest = build_manifest(&inputs);
        assert_eq!(manifest.unresolved_count, CAPABILITY_SPECS.len());
    }

    // ------------------------------------------------------------------
    // The renderers.
    // ------------------------------------------------------------------

    /// Every `CAPABILITY_SPECS` id — plus its class, description, anchor and
    /// expected rungs — appears in the generated doc. Mirrors
    /// `coord_doctor::tests::onboarding_doc_mentions_every_spec_name_and_fix`:
    /// one source of truth means the doc cannot claim a row the manifest does
    /// not have, and cannot omit one it does.
    #[test]
    fn manifest_doc_mentions_every_capability_and_every_rung() {
        let doc = render_manifest_doc();
        for spec in CAPABILITY_SPECS {
            assert!(
                doc.contains(spec.id),
                "doc missing capability id {:?}",
                spec.id
            );
            assert!(
                doc.contains(spec.class),
                "doc missing class {:?} for {:?}",
                spec.class,
                spec.id
            );
            assert!(
                doc.contains(spec.description),
                "doc missing description for {:?}",
                spec.id
            );
            assert!(
                doc.contains(spec.anchor),
                "doc missing anchor {:?} for {:?}",
                spec.anchor,
                spec.id
            );
            assert!(
                doc.contains(&expected(spec)),
                "doc missing expected rungs for {:?}",
                spec.id
            );
        }
        for rung in Rung::ALL {
            assert!(
                doc.contains(rung.wire()),
                "doc missing rung {:?}",
                rung.wire()
            );
            assert!(
                doc.contains(rung.describe()),
                "doc missing description for rung {:?}",
                rung.wire()
            );
        }
        // The do-not-edit banner, so the checked-in file warns editors to
        // regenerate instead of hand-editing.
        assert!(doc.contains("GENERATED"));
        assert!(doc.contains("--capability-manifest-doc"));
        assert!(doc.contains("CAPABILITY_SPECS"));
        // The two non-answers must be distinguished in the DOC, not only in the
        // source — a reader who conflates them misreads every manifest.
        assert!(doc.contains("are **not** synonyms"));
    }

    /// The doc is a pure function of the two tables: identical across calls, and
    /// carrying no runtime value. That is what makes a freshness workflow able
    /// to gate it at all.
    #[test]
    fn manifest_doc_is_byte_stable_and_carries_no_runtime_value() {
        assert_eq!(render_manifest_doc(), render_manifest_doc());
        let doc = render_manifest_doc();
        assert!(
            !doc.contains(env!("QONTINUI_GIT_SHA")),
            "the generated doc must not embed this build's git sha — it would \
             churn on every commit and could never be gated"
        );
    }

    /// The text render names every capability and its rung, and prints a
    /// `rejected` line for a successful row that carries one.
    #[test]
    fn text_render_lists_every_row_and_prints_rejections_on_success() {
        let mut inputs = unobserved_inputs();
        inputs.workspace_root = Some(CapabilityObservation::from_workspace_root_kind(
            WorkspaceRootKind::Discovered,
            Some("/src".to_string()),
            Some("$QONTINUI_ROOT is blank".to_string()),
        ));
        let text = build_manifest(&inputs).render();

        for spec in CAPABILITY_SPECS {
            assert!(text.contains(spec.id), "render missing {:?}", spec.id);
        }
        assert!(text.contains("exe_relative_checkout"));
        assert!(text.contains("rejected:      $QONTINUI_ROOT is blank"));
        assert!(text.contains("detail:        WorkspaceRootKind::discovered"));
        assert!(text.contains("schema_version: 1"));
    }

    /// `rank` is `ALL`'s own ordering, and the two non-answers rank worst —
    /// which is what makes "report the worst of several probed assets"
    /// conservative rather than arbitrary.
    #[test]
    fn rung_rank_is_the_declared_ordering_with_the_non_answers_last() {
        for (i, rung) in Rung::ALL.iter().enumerate() {
            assert_eq!(
                rung.rank(),
                i,
                "rung {:?} ranks off its ALL slot",
                rung.wire()
            );
        }
        assert!(Rung::Embedded.rank() < Rung::BundleResource.rank());
        assert!(Rung::BundleResource.rank() < Rung::DevCheckout.rank());
        assert!(Rung::OperatorCheckout.rank() < Rung::Unresolved.rank());
        assert!(Rung::Unresolved.rank() < Rung::Unknown.rank());
    }

    // ------------------------------------------------------------------
    // The Phase 4 driver.
    // ------------------------------------------------------------------

    /// **The Phase 4 gate.** `observed_here` fills the two rows it can probe
    /// with a REAL rung, and leaves every row it cannot observe from here as
    /// `unknown` naming its owning symbol — never a plausible value.
    ///
    /// Deliberately asserts the SPLIT rather than specific rungs: which rung
    /// `workspace_root` answers on depends on the machine running the suite,
    /// and pinning it would make this test a statement about the box.
    #[test]
    fn observed_here_probes_what_it_can_and_states_unknown_for_the_rest() {
        let _guard = store_lock();
        reset_provision_store();

        let manifest = build_manifest(&ManifestInputs::observed_here());
        assert_eq!(manifest.rows.len(), CAPABILITY_SPECS.len());

        // `workspace_root` is probed live and read-only, so it always carries a
        // real verdict — never `Unknown`, which would mean nothing looked.
        let root = manifest.row("workspace_root").expect("row present");
        assert_ne!(
            root.rung,
            Rung::Unknown,
            "`workspace_root` is probed by `observed_here`; `unknown` there means the \
             driver stopped calling `workspace_paths::workspace_root_observation`"
        );
        assert!(root
            .detail
            .as_deref()
            .is_some_and(|d| d.starts_with("WorkspaceRootKind::")));

        // `bundled_resources` is probed too, and always states its per-asset
        // findings — whether or not the bundle rung was reachable from a test
        // process (it is not: there is no Tauri `AppHandle`).
        let bundled = manifest.row("bundled_resources").expect("row present");
        assert!(bundled
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("asset(s) probed")));

        // `spec_pages` is asserted CONDITIONALLY, not as `unknown`. The
        // provisioning store is process-wide and `spec_api::storage`'s own unit
        // tests exercise the bare read wrappers, which record a real arm — so a
        // parallel `cargo test` can legitimately leave a reading here. What must
        // hold either way is the discipline: unknown NAMES the anchor, and a
        // non-unknown row is a real arm rather than an invented one.
        let specs = manifest.row("spec_pages").expect("row present");
        if specs.rung == Rung::Unknown {
            assert!(specs
                .note
                .as_deref()
                .is_some_and(|n| n.contains(capability("spec_pages").anchor)));
        } else {
            assert!(
                matches!(
                    specs.rung,
                    Rung::OperatorCheckout | Rung::Embedded | Rung::Unresolved
                ),
                "spec_pages can only report the arms `spec_api::storage` has, got {:?}",
                specs.rung.wire()
            );
        }

        // Everything the driver cannot observe from here stays `unknown` AND
        // names its anchor. With an empty ledger that is the other six rows;
        // nothing outside this module's own tests records them, so they are
        // deterministic.
        for id in [
            "fleet_commands",
            "fleet_skills",
            "fleet_agents",
            "agent_definitions",
            "agent_commands_registry",
            "slash_commands",
        ] {
            let row = manifest.row(id).expect("row present");
            assert_eq!(
                row.rung,
                Rung::Unknown,
                "nothing has provisioned or read in this process, so {id} must be unknown"
            );
            assert!(
                row.note
                    .as_deref()
                    .is_some_and(|n| n.contains(capability(id).anchor)),
                "{id}'s unknown note must name its owning symbol"
            );
        }

        reset_provision_store();
    }

    /// The driver reads the ledger THROUGH `observed`, so a recorded pass
    /// reaches the manifest, while the two probed rows are still taken live.
    #[test]
    fn observed_here_carries_the_ledger_and_still_probes_live() {
        let _guard = store_lock();
        reset_provision_store();

        let workdir = "/tmp/agent-worktree-observed-here";
        let mut skills = ProvisionReport::new("fleet_skills", 2, Rung::Embedded);
        skills.record_written();
        skills.skip("visual-audit", SkipReason::GitTracked);
        record_provision(workdir, skills);
        // A non-provisioning observation reaches the driver the same way.
        // (`fleet_agents` rather than `spec_pages`: the store is process-wide
        // and `spec_api::storage`'s tests write that key from another thread.)
        record_observation(
            "fleet_agents",
            CapabilityObservation::new(Rung::Embedded).with_detail("embedded floor"),
        );

        let manifest = build_manifest(&ManifestInputs::observed_here());
        assert_eq!(manifest.row("fleet_skills").unwrap().rung, Rung::Embedded);
        assert!(manifest
            .row("fleet_skills")
            .unwrap()
            .note
            .as_deref()
            .is_some_and(|n| n.contains("visual-audit")));
        assert_eq!(manifest.row("fleet_agents").unwrap().rung, Rung::Embedded);
        assert_ne!(manifest.row("workspace_root").unwrap().rung, Rung::Unknown);

        reset_provision_store();
    }

    /// The two stale rows Phase 2 flagged, pinned so they cannot silently
    /// regress: `bundled_resources`' anchor names the symbol that actually
    /// produces the observation, and `spec_pages` no longer claims a rung its
    /// resolver cannot emit.
    #[test]
    fn the_roster_matches_the_resolvers_phase_2_actually_shipped() {
        let bundled = capability("bundled_resources");
        assert!(
            bundled
                .anchor
                .contains("bundled_resources::resolve_with_rung"),
            "the anchor must name the symbol that produces the observation, got {:?}",
            bundled.anchor
        );

        let specs = capability("spec_pages");
        assert!(
            !specs.expected_rungs.contains(&Rung::DevCheckout),
            "Phase 2's mapping emits `operator_checkout` for the filesystem arm — the \
             caller's root is any registered app's `<repo_root>/specs`, so `dev_checkout` \
             is a rung this capability can never report"
        );
        assert_eq!(
            specs.expected_rungs,
            &[Rung::OperatorCheckout, Rung::Embedded, Rung::Unresolved]
        );
    }

    /// The JSON is well-formed, keyed as the wire contract says, and encodes
    /// rungs as their wire strings.
    #[test]
    fn manifest_json_is_wellformed_and_uses_wire_strings() {
        let mut inputs = unobserved_inputs();
        inputs.fleet_skills = Some(CapabilityObservation::new(Rung::Embedded));
        let manifest = build_manifest(&inputs);
        let json = render_manifest_json(&manifest);

        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["git_sha"], "0123456789ab");
        assert_eq!(parsed["app_version"], "1.0.10");
        assert_eq!(
            parsed["rows"].as_array().map(Vec::len),
            Some(CAPABILITY_SPECS.len())
        );
        assert_eq!(parsed["unresolved_count"], CAPABILITY_SPECS.len() - 1);

        let rows = parsed["rows"].as_array().expect("rows array");
        assert_eq!(rows[0]["id"], "workspace_root");
        assert_eq!(rows[0]["rung"], "unknown");
        let skills = rows
            .iter()
            .find(|r| r["id"] == "fleet_skills")
            .expect("fleet_skills row");
        assert_eq!(skills["rung"], "embedded");
    }
}
