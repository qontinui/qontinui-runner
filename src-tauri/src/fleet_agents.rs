//! The named-subagent definitions this binary ships.
//!
//! Third sibling of [`crate::fleet_commands`] (`.claude/commands/*.md`) and
//! [`crate::fleet_skills`] (`.claude/skills/<name>/SKILL.md`). This one covers
//! `.claude/agents/*.md` — the definitions `claude` reads to resolve a named
//! subagent, e.g. the `merge-specialist` an auto-spawned review prompt invokes,
//! or the `code-reviewer` the fleet's pre-PR-review policy names.
//!
//! ## Why this exists
//!
//! `agent_runtime::provision_agent_definitions` COPIES these from a
//! `qontinui-claude-config` checkout, and its own docstring has named the fix
//! ever since:
//!
//! > *The fleet-portability follow-up is to BUNDLE these defs into the runner
//! > binary (`include_str!`) so non-operator devices without a
//! > `qontinui-claude-config` checkout still get them; this copy-from-checkout
//! > path unblocks the current operator fleet.*
//!
//! Without a checkout the copy is a no-op that `warn!`s and returns `Ok`, so a
//! spawned agent silently has no subagents: `claude` cannot resolve the named
//! subagent, the review never runs, and coord eventually ages the PR out as
//! `specialist_timeout`. That is a failure with no error at the point of cause.
//!
//! ## Checkout still WINS — this is a floor, not a replacement
//!
//! [`provision_agent_definitions_from_root`] now writes these embedded defaults
//! FIRST and then overlays any checkout copies on top. An operator editing
//! `qontinui-claude-config/.claude/agents/*.md` keeps the live-edit workflow
//! they have today, byte for byte (a RETIRED definition excepted: the overlay
//! never delivers one); a device with no checkout gets the embedded set instead
//! of nothing.
//!
//! **Both layers yield to the destination's own repository.** A definition the
//! enclosing repo tracks is kept as it is, and so is an existing one when the
//! tracked-file probe could not answer inside a repo. That is decided once,
//! before either layer writes ([`crate::provision_guard::KeepDecisions`]). So in
//! a `qontinui-claude-config` worktree the session resolves that worktree's own
//! commit of a tracked definition, not the primary checkout's.
//!
//! ## The `.md` files in `fleet_agents/` are a RENDER, not the source of truth
//!
//! They are ordinary files in this public repository, reviewed through a normal
//! pull request, with git history as the tamper record. But the CONTENT
//! originates in `qontinui-claude-config/.claude/agents/*.md` — the copy the
//! checkout overlay above writes on top of these, and the one humans edit.
//! Edit a definition THERE and copy it forward byte for byte; an edit made
//! directly here is drift the moment it lands. Same direction as
//! [`crate::fleet_skills`], whose header was corrected the same way.
//!
//! This header used to call these files "the CANONICAL sources", and nothing
//! carried config edits across: from the bundle's creation (`0245c2e96`,
//! 2026-08-28) to its first full re-sync, `merge-specialist.md` drifted ~700
//! lines and kept telling runner-provisioned specialists about a coord executor
//! that no longer acts on their decision (coord finding `491c906f`). That gap
//! is closed in `qontinui-claude-config`: `scripts/fleet-bundle-sync.sh`
//! carries this directory from its source (it joined the sync's bundle pairs on
//! 2026-09-24), and `.github/workflows/fleet-agents-parity.yml` gates it hourly
//! through `scripts/agent-bundle-parity.sh` (ccfg#1152). Until 2026-11-07 this
//! paragraph still said no check covered it.
//!
//! ## Retiring a definition starts here
//!
//! Neither of those deletes anything. The sync never removes a bundled file,
//! and the gate reads a bundled file with no source as a `SOURCE_ABSENT`
//! violation, so a definition leaves this directory FIRST (`git rm` its `.md`
//! and add its line to [`RETIRED_AGENTS`]) and its source second. `git rm`
//! stops it shipping, but a copy this binary, an older one or a checkout
//! overlay already wrote stays in every workdir it reached.
//! [`remove_retired_agents`] removes that copy on the next provisioning pass,
//! when its git blob id is one the definition shipped as. A copy with any other
//! blob is somebody's edit, so it is kept.
//!
//! Because they ship to every fleet device they must stay free of any one
//! operator's absolute paths — see
//! [`tests::bundled_agent_defs_have_no_operator_local_paths`].

use std::path::Path;

use include_dir::{include_dir, Dir};

use crate::capability_manifest::{self, ProvisionReport, RetainedReason, SkipReason};
use crate::provision_guard::KeepDecisions;

/// The embedded subagent definitions. A flat directory of `*.md`, matching what
/// `claude` expects under `.claude/agents/`.
///
/// A [`Dir`] rather than [`crate::fleet_commands::FLEET_COMMANDS`]' explicit
/// `(name, body)` roster: these are all one shape with no per-entry wiring, so
/// **adding an agent is adding a `.md` file here and nothing else**. Same
/// reasoning as [`crate::fleet_skills`], and the same crate already backs
/// `spec_api::storage::EMBEDDED_PAGES`.
static FLEET_AGENTS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/fleet_agents");

/// Write every embedded subagent definition into `dst_dir`, returning a
/// [`ProvisionReport`] describing what landed. Creates `dst_dir` if absent;
/// overwrites existing files (idempotent) EXCEPT a destination that is a
/// symlink (never written through) and those `keep` names: a
/// definition the enclosing git repository tracks (or one that exists when the
/// tracked-file probe could not answer inside a repository) is left alone and
/// reported as skipped, exactly as [`crate::fleet_commands`] and
/// [`crate::fleet_skills`] do. In a `qontinui-claude-config` checkout
/// `.claude/agents/*.md` IS tracked content, and overwriting it with this
/// binary's render dirtied every such worktree. The provisioner also folds
/// [`foreign_destination_reason`] into `keep` before either layer writes, so an
/// existing UNTRACKED file that is not a copy this runner shipped is kept too.
///
/// Only `*.md` at the top level is written — the same filter the checkout copy
/// applies, so the two layers cannot disagree about what counts as a definition.
///
/// It removes nothing, and neither layer writes a RETIRED name: none is
/// bundled, and the checkout overlay skips them. Removing a copy an older binary
/// left is [`remove_retired_agents`]'s job, which the caller runs after both
/// layers.
///
/// **The report changes no behaviour.** This function was fail-soft from its
/// caller's side already (`agent_runtime` catches the `Err`, warns, and
/// continues to the checkout overlay); it used to return a bare `usize`, which
/// could say how many defs landed but never which ones did not, or why. A
/// definition that never lands is the silent failure this module exists to
/// remove — `claude` cannot resolve the named subagent, the review never runs,
/// and coord ages the PR out as `specialist_timeout` with no error at the point
/// of cause — so "how many" was never the interesting half.
pub(crate) fn provision_fleet_agents_into(
    dst_dir: &Path,
    keep: &KeepDecisions,
) -> std::io::Result<ProvisionReport> {
    std::fs::create_dir_all(dst_dir)?;
    let mut out = ProvisionReport::new(
        "fleet_agents",
        embedded_agent_count(),
        capability_manifest::Rung::Embedded,
    )
    .with_destination(dst_dir.display().to_string());
    for file in FLEET_AGENTS.files() {
        if file.path().extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = file.path().file_name() else {
            continue;
        };
        let unit = name.to_string_lossy().into_owned();
        if let Some(reason) = keep.reason(&unit) {
            tracing::info!(
                "fleet_agents: keeping {} as it is on disk — {}",
                dst_dir.join(name).display(),
                reason.describe()
            );
            out.skip(unit, reason.clone());
            continue;
        }
        // Never write THROUGH a symlink: `fs::write` follows it and would
        // replace whatever file it points at, which this pass does not own.
        let dst = dst_dir.join(name);
        if is_symlink(&dst) {
            let reason = symlink_destination_reason(&dst);
            tracing::info!(
                "fleet_agents: not writing {} — {}",
                dst.display(),
                reason.describe()
            );
            out.skip(unit, reason);
            continue;
        }
        // Per-file rather than `?`: one unwritable definition must not cost the
        // session the others, and it must be NAMED rather than aborting the
        // pass at whatever point it happened to reach. Atomic: a symlink or
        // hard link that appeared since the check above is REPLACED as a
        // directory entry, never written through.
        match write_atomically(&dst, file.contents(), None) {
            Ok(()) => out.record_written(),
            Err(e) => out.skip(unit, SkipReason::WriteFailed(e.to_string())),
        }
    }
    if out.written == 0 {
        out.set_rung(capability_manifest::Rung::Unresolved);
    }
    Ok(out)
}

/// File names of the embedded subagent definitions (`*.md` only), the
/// candidates [`KeepDecisions::decide`] is asked about.
pub(crate) fn embedded_agent_names() -> Vec<String> {
    FLEET_AGENTS
        .files()
        .filter(|f| f.path().extension().and_then(|e| e.to_str()) == Some("md"))
        .filter_map(|f| f.path().file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .collect()
}

/// Number of embedded subagent definitions, for log lines that want to say how
/// many defaults were available independent of how many the checkout overlaid.
pub(crate) fn embedded_agent_count() -> usize {
    FLEET_AGENTS
        .files()
        .filter(|f| f.path().extension().and_then(|e| e.to_str()) == Some("md"))
        .count()
}

/// Every subagent definition this binary has RETIRED. One line per file: the
/// file name with its `.md`, then every git blob id the file ever shipped as,
/// separated by single spaces. The installer's half of the same retirement, the
/// qontinui-claude-config PR that deletes the source file, will give
/// `scripts/install-agents.sh` a `RETIRED_AGENTS` table of the same shape, with
/// a file's line byte-identical to the one here.
///
/// A line lists the blobs of the SOURCE's whole history, not only the ones this
/// directory bundled, because an older binary's checkout overlay copied from the
/// source and a workdir can hold any of them, including a blob that only ever sat on an
/// open PR's branch a checkout was standing on. A line only grows: the bundle sync never
/// re-adds a deleted file, so a blob that ships after the line is written is
/// never learned here.
pub(crate) const RETIRED_AGENTS: &[&str] = &[
    // Retired by plan
    // 2026-11-06-what-the-fleet-records-and-advertises-matches-what-actually-ran,
    // Phase 6: nothing dispatched it, and its body contradicted served policy
    // (`testing` coverage-is-enumerated-not-salient and a-test-must-be-able-to-fail;
    // `escalation-bar` escalation-closed-list).
    // The first five blobs are its whole history on qontinui-claude-config main,
    // of which this directory shipped 8c23f14a, d80c5d1d and 3908f6b9. The next
    // two are its blobs on the branch of qontinui-claude-config#2413 (c0648db7,
    // 4a60b2d4f3d4; its merge commit adc57b9e adds none), which was open and
    // editing it when it was retired. The eighth is its blob after
    // qontinui-claude-config#2528 (716c4f187, 2026-11-09), which superseded
    // #2413 and landed an edit to it on main before the source deletion did.
    "test-generator.md 3908f6b9811fea0b8f7bc68f7d0bf9022015a537 d80c5d1d620bb8f9e2acdd756eadc22b624794a0 8c23f14a7cbaaf04d35f68ad13745242f91012b6 d8d6673b504bb9f37155ec0acecb8fc02859ae95 7b5318b48ebfaaeaba1aaff7d86f99c0bbb07535 0bb59a9cea7f0a8c598f648e40f27300bcda9dde 7b6360157dc99a9c86919343a0cc3a65b8ee6091 565d8136ded8048c22b8ba3c5b4e771a1f6e2aac",
];

/// `line` split into its file name and the blob ids after it.
fn parse_retired_line(line: &str) -> (&str, Vec<&str>) {
    let mut fields = line.split(' ');
    let name = fields.next().unwrap_or_default();
    (name, fields.collect())
}

/// File names of the definitions `retired` lists. They are candidates
/// [`KeepDecisions::decide`] must be asked about, so a retired copy the
/// enclosing repository tracks is kept like any other tracked definition.
pub(crate) fn retired_agent_names(retired: &[&str]) -> Vec<String> {
    retired
        .iter()
        .map(|line| parse_retired_line(line).0.to_string())
        .collect()
}

/// Remove from `<workdir>/.claude/agents` every definition `retired` lists
/// whose copy on disk is a blob it shipped as, and record each removal on
/// `report`.
///
/// Only the workdir's own files are ever removed: when `.claude` or
/// `.claude/agents` is reached through a symlink (the directory does not
/// canonicalize to `<canonical workdir>/.claude/agents`), a shipped copy there
/// is kept and recorded as [`RetainedReason::LinkedElsewhere`]. The workspace
/// root's `.claude` is such a link into qontinui-claude-config, whose copies
/// are that repository's own.
///
/// What stays, by arm:
/// - **absent**: nothing to do;
/// - **kept by `keep`**: a copy the enclosing repository tracks, or one that
///   existed when the tracked-file probe could not answer inside a repository,
///   stays exactly as both provisioning layers keep it (`keep` was decided
///   before either layer wrote), and is recorded as
///   [`RetainedReason::KeptByRepository`];
/// - **any other blob**: somebody edited it, so it is kept, logged and
///   recorded as [`RetainedReason::LocallyModified`]. That is the
///   own-files-only rule `install-agents.sh --uninstall` applies;
/// - **unreadable or unremovable**: kept, and recorded as
///   [`RetainedReason::ReadFailed`] / [`RetainedReason::RemoveFailed`], so the
///   pass reads degraded and names a retired definition that may still resolve. A
///   copy that vanishes between the read and the removal (a concurrent pass
///   into the same workdir) is already gone and is not recorded.
///
/// A blob is matched on the bytes as they are and on the same bytes with CRLF
/// folded to LF. A checkout with `core.autocrlf=true` holds a CRLF copy of a
/// blob git stores as LF, and that copy is the shipped content, not an edit
/// (`agent-bundle-parity.sh` compares through the same fold).
///
/// It never fails and never panics: a pass that could not finish still leaves
/// the spawn running, as with both layers before it.
pub(crate) fn remove_retired_agents(
    workdir: &Path,
    keep: &KeepDecisions,
    retired: &[&str],
    report: &mut ProvisionReport,
) {
    let dst_dir = workdir.join(".claude").join("agents");
    let linked = match (dst_dir.canonicalize(), workdir.canonicalize()) {
        (Ok(dst), Ok(root)) => {
            (dst != root.join(".claude").join("agents")).then(|| dst.display().to_string())
        }
        // No agents directory: nothing retired can be on disk.
        _ => return,
    };
    for line in retired {
        let (name, shipped) = parse_retired_line(line);
        // This function deletes files, so it trusts no name: exactly one plain
        // path component, or nothing happens (a `C:x.md` or `../x.md` would
        // otherwise make `join` leave the agents directory).
        let mut components = Path::new(name).components();
        if !matches!(
            (components.next(), components.next()),
            (Some(std::path::Component::Normal(_)), None)
        ) {
            tracing::warn!("fleet_agents: ignoring retired entry {name:?}: not a plain file name");
            continue;
        }
        let path = dst_dir.join(name);
        // A RootVersioned keep says "do not CREATE this" — it is never a reason
        // to keep something on disk (a dangling symlink at a retired name
        // would otherwise be recorded as kept by the repository).
        let keep_reason = keep
            .reason(name)
            .filter(|r| **r != crate::capability_manifest::SkipReason::RootVersioned);
        if let Some(reason) = keep_reason {
            if std::fs::symlink_metadata(&path).is_ok() {
                tracing::info!(
                    "fleet_agents: keeping retired {} as it is on disk — {}",
                    path.display(),
                    reason.describe()
                );
                report.record_retained_retired(
                    name,
                    RetainedReason::KeptByRepository(reason.wire().to_string()),
                );
            }
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                tracing::warn!(
                    "fleet_agents: could not read retired {}, so it is kept: {e}",
                    path.display()
                );
                report.record_retained_retired(name, RetainedReason::ReadFailed(e.to_string()));
                continue;
            }
        };
        let blob = match shipped_blob(&bytes, &shipped) {
            Ok(blob) => blob,
            Err(own) => {
                tracing::warn!(
                    "fleet_agents: keeping retired {} — locally modified (blob {own} is not \
                     one it shipped as)",
                    path.display()
                );
                report.record_retained_retired(name, RetainedReason::LocallyModified(own));
                continue;
            }
        };
        if let Some(resolved) = &linked {
            tracing::warn!(
                "fleet_agents: keeping retired {} — .claude/agents is reached through a \
                 symlink, to {resolved}",
                path.display()
            );
            report.record_retained_retired(name, RetainedReason::LinkedElsewhere(resolved.clone()));
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!(
                    "fleet_agents: removed retired {} (blob {blob}, one it shipped as)",
                    path.display()
                );
                report.record_removed(name, blob);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(
                    "fleet_agents: retired {} was already removed by a concurrent pass",
                    path.display()
                );
            }
            Err(e) => {
                tracing::warn!(
                    "fleet_agents: could not remove retired {}: {e}",
                    path.display()
                );
                report.record_retained_retired(name, RetainedReason::RemoveFailed(e.to_string()));
            }
        }
    }
}

/// Every blob each BUNDLED definition has shipped as, one line per file in
/// [`RETIRED_AGENTS`]' shape: the file name, then the blob ids, space-separated.
/// A line lists the bundle's own history in this repository (every branch) and
/// its source's history on `qontinui-claude-config` main, because a workdir can
/// hold a copy an older binary's floor wrote or an older checkout overlay
/// copied.
///
/// It is how the provisioner tells ITS OWN copy from an operator's file at the
/// same name ([`foreign_destination_reason`]): a destination whose bytes are one
/// of these blobs is refreshed, any other existing file is kept. A blob missing
/// here would freeze a stale copy in place rather than clobber anything, and
/// [`tests::every_bundled_definition_is_in_the_shipped_table`] fails the build
/// when the bundle's current blob is not listed, so a re-sync must add its line.
pub(crate) const SHIPPED_AGENTS: &[&str] = &[
    "code-reviewer.md 519fd1ab38bc53a7514bbbe20f60ae01957abcb0 624b3b339ebc4ae13850c124e3992d162b788e49 6b6e6f08312a2e5a581c139415a81b3c5d5ce3f4 959d627662996f8bde590a13a6c84ad22f3a8529 b28243932fb683f13285c11a6f190c69b89d2949 d895a90a12c5480816fed332958bc009a9ae88ad",
    "debugging-specialist.md 1d1851df238088b5d0b646470bdc0a54b48d9593 4197dd492b8e9ead49ae1486e49ea670122ee7bf a9e6b6e25535c13ad85e50dccafa4b2178e1e20e d10848df68d3731545f0f740ef8979d2e7fd5a7c f8d86dae7e6e5b1bcd9e995678f30f3c01ed6e06",
    "merge-specialist.md 2c2c6b8f16c8c6d802031bd633a07a2bb1c39c06 34d940fae8d8dea82c2edb6f71d935b49d67927e 61496d273875945aa0d978c6c1acf3bf8d087e56 647e942b3b9e31ac2ce85c24202d4ad3db75e8b9 cbced9b9855b4f21dc3310872ad1a6ec54640b82 ce8ecbe3210ce14497b5a57d47fd6a3dff78c2cd d22a06aa0b0313d77091d910313cf30d11995bff e094d49328098d0198a210b4901a224904081f78 e5ce959417a0ea4fd89c3d5c3ed4739f22181312 f35d49a5fddc93fbf6d3090106c6b3d0a5e6069f",
    "repo-auditor.md 45954e716979d2d6e6c6ecc7d1616db1ff19f58e 5781f39d5c0a2f7abe9455b661fd72cde6784a8e 64764a4119d9277a1ce58a09ad7e683cd2111367 6e3b88232bd59f0569aa8ab459d8f6396a025b1e a877ff7aa55ec01b3a29ed08250aa0f32b85c7c6 aba1edddfec2c7f0806685ba9cc184f2439b0df3 e5ec73ffb9ab0389102e6f737ac10ea288f9571d",
];

/// How long the overlay checkout's history may take to say whether its
/// definition's path held a blob before the probe is taken as UNANSWERED (and
/// the file kept). A whole-history `git log --all` over a large repository is
/// not a quick read, so this is sized for that walk rather than for a lookup.
const BLOB_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// What [`checkout_history_holds_blob`] learned. Only `Yes` licenses an
/// overwrite; `No` and `NoAnswer` both keep the file, but they are recorded
/// differently: `No` is a definite "not ours", `NoAnswer` is UNKNOWN.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HistoryProbe {
    /// The path's history holds one of the blobs.
    Yes,
    /// The probe answered cleanly and the history holds none of them.
    No,
    /// The probe gave no usable answer; `why` is `timeout`, `exit <N>`,
    /// `killed by signal`, `spawn failed (<error>)` or `wait failed (<error>)`.
    NoAnswer { why: String },
}

/// Why the existing destination `dst` (definition `name`) must be KEPT rather
/// than overwritten, or `None` when this pass may write it: it is absent, or its
/// bytes are a copy this runner ships or shipped.
///
/// The tracked-file guard ([`KeepDecisions::decide`]) keeps only what a
/// repository tracks, so before this an operator's own UNTRACKED file at a
/// bundled or overlay name (`~/.claude/agents/code-reviewer.md` for a terminal
/// opened at `$HOME`, a gitignored `.claude/agents/*.md`) was silently replaced.
/// A copy counts as this runner's when its bytes, as they are or with CRLF
/// folded to LF, are
/// - the bundled body, or a blob [`SHIPPED_AGENTS`] or `retired` lists for it;
/// - byte-equal to the overlay checkout's current copy (`overlay_src`); or
/// - a blob the overlay checkout's history holds AT THIS DEFINITION'S PATH
///   (`.claude/agents/<name>` on any ref), i.e. a version of that checkout's
///   own definition. The same bytes at a different path are not evidence, so
///   they do not count ([`checkout_history_holds_blob`]; bounded by
///   [`BLOB_PROBE_TIMEOUT`], never lazily fetched). A probe that gives no
///   answer keeps the file, the safe side, and the detail says the ownership
///   probe did not answer rather than claiming the blob is not ours.
///
/// A symlink is never written through, whatever it points at, and anything that
/// cannot be inspected or read is kept: overwriting needs a positive answer.
/// Every keep is a [`SkipReason::OperatorAuthored`] whose detail says which.
pub(crate) fn foreign_destination_reason(
    dst: &Path,
    name: &str,
    overlay_src: Option<&Path>,
    retired: &[&str],
) -> Option<SkipReason> {
    foreign_destination_reason_with(dst, name, overlay_src, retired, SHIPPED_AGENTS)
}

/// [`foreign_destination_reason`] with the shipped-blob table passed in, so a
/// test can name a blob it computed itself.
fn foreign_destination_reason_with(
    dst: &Path,
    name: &str,
    overlay_src: Option<&Path>,
    retired: &[&str],
    shipped_table: &[&str],
) -> Option<SkipReason> {
    let meta = match std::fs::symlink_metadata(dst) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(SkipReason::OperatorAuthored(format!(
                "the existing destination could not be inspected ({e}), so it was not \
                 overwritten"
            )))
        }
    };
    if meta.file_type().is_symlink() {
        return Some(symlink_destination_reason(dst));
    }
    if !meta.is_file() {
        return Some(SkipReason::OperatorAuthored(
            "the destination exists and is not a regular file".to_string(),
        ));
    }
    let bytes = match std::fs::read(dst) {
        Ok(bytes) => bytes,
        Err(e) => {
            return Some(SkipReason::OperatorAuthored(format!(
                "the existing destination could not be read ({e}), so it was not \
                 overwritten"
            )))
        }
    };
    let folded = fold_crlf(&bytes);
    let bundled = FLEET_AGENTS
        .files()
        .find(|f| f.path().file_name().is_some_and(|n| n == name))
        .map(|f| f.contents());
    if bundled.is_some_and(|b| b == bytes.as_slice() || b == folded.as_slice()) {
        return None;
    }
    let shipped: Vec<&str> = shipped_table
        .iter()
        .chain(retired)
        .map(|line| parse_retired_line(line))
        .filter(|(n, _)| *n == name)
        .flat_map(|(_, blobs)| blobs)
        .collect();
    let own = match shipped_blob(&bytes, &shipped) {
        Ok(_) => return None,
        Err(own) => own,
    };
    if let Some(src_dir) = overlay_src {
        if let Ok(source) = std::fs::read(src_dir.join(name)) {
            if source == bytes || fold_crlf(&source) == folded {
                return None;
            }
        }
        // One probe for both spellings, and the folded blob only when folding
        // changed anything: an LF file's folded blob IS its own.
        let mut blobs = vec![own.clone()];
        if folded != bytes {
            blobs.push(crate::provenance::git_blob_id(&folded));
        }
        match checkout_history_holds_blob(src_dir, name, &blobs) {
            HistoryProbe::Yes => return None,
            HistoryProbe::No => {}
            HistoryProbe::NoAnswer { why } => {
                return Some(SkipReason::OperatorAuthored(format!(
                    "ownership probe did not answer ({why}); blob {own} is not one \
                     this runner ships, and whether the overlay checkout's history holds \
                     it is UNKNOWN"
                )))
            }
        }
    }
    Some(SkipReason::OperatorAuthored(format!(
        "an operator-authored file not written by this runner (blob {own} is not one it \
         ships or shipped)"
    )))
}

/// The skip reason for a destination that is a symbolic link: this runner never
/// writes THROUGH one, since the write would land on whatever it points at.
pub(crate) fn symlink_destination_reason(dst: &Path) -> SkipReason {
    let target = std::fs::read_link(dst)
        .map(|t| t.display().to_string())
        .unwrap_or_else(|e| format!("<unreadable link: {e}>"));
    SkipReason::OperatorAuthored(format!(
        "the destination is a symbolic link (to {target}); this runner never writes through \
         one"
    ))
}

/// True when `dst` is a symbolic link (dangling or not).
pub(crate) fn is_symlink(dst: &Path) -> bool {
    std::fs::symlink_metadata(dst).is_ok_and(|m| m.file_type().is_symlink())
}

/// True when the history of definition `name` in the git repository holding
/// `dir` (the overlay's `.claude/agents`) has, at THAT path, one of `blobs`: a
/// version the overlay checkout itself carried for this definition on some ref.
///
/// Scoped to the path on purpose. `git cat-file -e` would accept the blob as
/// any object anywhere in the repository, so an operator's file whose bytes
/// happen to equal some other tracked file (another definition, a fixture)
/// would read as ours and be overwritten. `git log --all --raw -- <name>` lists
/// only the blobs that path has held, old and new side of every change.
///
/// `GIT_NO_LAZY_FETCH=1`: on a partial clone a missing object would otherwise
/// be fetched from the remote inside this probe. Bounded by
/// [`BLOB_PROBE_TIMEOUT`]. Only a clean exit gives `Yes`/`No`; no repository,
/// git missing, a timeout or a non-zero exit is [`HistoryProbe::NoAnswer`],
/// which keeps the file and is recorded as UNKNOWN.
fn checkout_history_holds_blob(dir: &Path, name: &str, blobs: &[String]) -> HistoryProbe {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = match crate::process_helpers::no_window("git")
        .arg("-C")
        .arg(dir)
        .args(["log", "--all", "--format=", "--raw", "--no-abbrev", "--"])
        // A literal pathspec, relative to `dir`: `name` is a file name, never a
        // glob.
        .arg(format!(":(literal){name}"))
        .env("GIT_NO_LAZY_FETCH", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return HistoryProbe::NoAnswer {
                why: format!("spawn failed ({e})"),
            }
        }
    };
    // Drained on its own thread so a long history cannot fill the pipe and
    // stall the child past the deadline.
    let reader = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = out.read_to_string(&mut text);
            text
        })
    });
    let deadline = std::time::Instant::now() + BLOB_PROBE_TIMEOUT;
    let outcome: Result<(), String> = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(status)) => {
                break Err(match status.code() {
                    Some(code) => format!("exit {code}"),
                    None => "killed by signal".to_string(),
                })
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("timeout".to_string());
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("wait failed ({e})"));
            }
        }
    };
    let text = reader.and_then(|r| r.join().ok()).unwrap_or_default();
    match outcome {
        Ok(()) if raw_log_names_blob(&text, blobs) => HistoryProbe::Yes,
        Ok(()) => HistoryProbe::No,
        Err(why) => HistoryProbe::NoAnswer { why },
    }
}

/// True when a `git log --raw --no-abbrev` listing names one of `blobs` on
/// either side of a change. Each raw line is
/// `:<old mode> <new mode> <old blob> <new blob> <status>\t<path>…`.
fn raw_log_names_blob(text: &str, blobs: &[String]) -> bool {
    text.lines()
        .filter_map(|line| line.strip_prefix(':'))
        .filter_map(|line| line.split('\t').next())
        .any(|meta| {
            let fields: Vec<&str> = meta.split(' ').collect();
            fields.len() >= 4 && blobs.iter().any(|b| b == fields[2] || b == fields[3])
        })
}

/// Write `bytes` to `dst` ATOMICALLY: into a temporary file in the same
/// directory, then renamed over `dst`. A rename REPLACES the directory entry,
/// so a destination that is (or became, after the caller checked) a symbolic
/// link or a hard link is replaced rather than written through, and a reader
/// never sees a half-written definition. On any error the temporary file is
/// removed (dropping an unpersisted [`tempfile::NamedTempFile`] deletes it).
///
/// `permissions` sets the new file's mode (the overlay passes its source's).
/// With `None`, on unix, a destination that already exists keeps its own mode
/// (the replacement inherits it); a new destination gets `0644`. Without this
/// the file would land `0600`, the mode a temporary file is created with.
pub(crate) fn write_atomically(
    dst: &Path,
    bytes: &[u8],
    permissions: Option<std::fs::Permissions>,
) -> std::io::Result<()> {
    use std::io::Write;
    let dir = dst.parent().unwrap_or_else(|| Path::new("."));
    let prefix = format!(
        ".{}.",
        dst.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    let mut tmp = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".tmp")
        .tempfile_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    match permissions {
        Some(perms) => tmp.as_file().set_permissions(perms)?,
        None => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // `metadata` follows a link, but a link destination is never
                // reached here (callers skip symlinks), and its target's mode
                // would be the honest one to keep anyway.
                let mode = std::fs::metadata(dst)
                    .map(|m| (m.permissions().mode() & 0o777) | 0o400)
                    .unwrap_or(0o644);
                tmp.as_file()
                    .set_permissions(std::fs::Permissions::from_mode(mode))?;
            }
        }
    }
    tmp.persist(dst).map(|_| ()).map_err(|e| e.error)
}

/// The blob in `shipped` that `bytes` is, as they are or with CRLF folded to
/// LF; otherwise `Err` carrying the bytes' own blob id, for the record.
fn shipped_blob(bytes: &[u8], shipped: &[&str]) -> Result<String, String> {
    let raw = crate::provenance::git_blob_id(bytes);
    if shipped.contains(&raw.as_str()) {
        return Ok(raw);
    }
    if bytes.windows(2).any(|pair| pair == b"\r\n") {
        let folded = crate::provenance::git_blob_id(&fold_crlf(bytes));
        if shipped.contains(&folded.as_str()) {
            return Ok(folded);
        }
    }
    Err(raw)
}

/// `bytes` with every CRLF pair folded to LF. A lone CR is kept.
fn fold_crlf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            continue;
        }
        out.push(b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisions_every_embedded_agent_def() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let dst = tmp.path().join(".claude").join("agents");

        let report =
            provision_fleet_agents_into(&dst, &KeepDecisions::default()).expect("provision");
        assert_eq!(
            report.written,
            embedded_agent_count(),
            "should write every embedded definition"
        );
        assert_eq!(report.expected, embedded_agent_count());
        assert!(report.skipped.is_empty(), "nothing should be skipped here");
        assert!(
            report.is_complete(),
            "a full pass must not read as degraded"
        );
        assert_eq!(report.rung, crate::capability_manifest::Rung::Embedded);
        assert!(report.written > 0, "the bundle should not be empty");

        for file in FLEET_AGENTS.files() {
            let name = file.path().file_name().expect("named");
            let path = dst.join(name);
            assert!(path.exists(), "{} should exist", path.display());
            let on_disk = std::fs::read(&path).expect("read provisioned def");
            assert_eq!(
                on_disk,
                file.contents(),
                "{} should be byte-identical to the embedded copy",
                path.display()
            );
        }
    }

    #[test]
    fn provision_is_idempotent_overwrite() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let dst = tmp.path().join(".claude").join("agents");

        let first = provision_fleet_agents_into(&dst, &KeepDecisions::default()).expect("first");
        let victim = dst.join("code-reviewer.md");
        std::fs::write(&victim, b"CLOBBERED").expect("clobber");

        let second = provision_fleet_agents_into(&dst, &KeepDecisions::default()).expect("second");
        assert_eq!(
            (first.written, first.skipped.len()),
            (second.written, second.skipped.len()),
            "both passes write the same count"
        );
        assert_ne!(
            std::fs::read(&victim).expect("read restored"),
            b"CLOBBERED",
            "re-provisioning must overwrite a modified def, not leave it"
        );
    }

    /// The bundle's current blob of every definition is in [`SHIPPED_AGENTS`],
    /// so a refreshed copy is recognised as this runner's on the next pass. A
    /// re-sync that changes a body must add the new blob to its line.
    #[test]
    fn every_bundled_definition_is_in_the_shipped_table() {
        for file in FLEET_AGENTS.files() {
            let name = file.path().file_name().unwrap().to_string_lossy();
            if !name.ends_with(".md") {
                continue;
            }
            let blob = crate::provenance::git_blob_id(file.contents());
            let listed = SHIPPED_AGENTS
                .iter()
                .map(|l| parse_retired_line(l))
                .any(|(n, blobs)| n == name && blobs.contains(&blob.as_str()));
            assert!(
                listed,
                "{name}'s bundled blob {blob} is missing from SHIPPED_AGENTS; add it to its line"
            );
        }
    }

    /// An existing file at a bundled name whose bytes this runner never shipped
    /// is an operator's, and is kept; one whose bytes are a shipped blob (as
    /// written or with CRLF folded) is ours and may be refreshed; an absent one
    /// is written.
    #[test]
    fn an_existing_destination_is_ours_only_when_its_blob_shipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dst = tmp.path().join("code-reviewer.md");
        assert_eq!(
            foreign_destination_reason_with(&dst, "code-reviewer.md", None, &[], &[]),
            None,
            "an absent destination is written"
        );

        let custom = b"---\nname: code-reviewer\n---\nmy own reviewer\n";
        std::fs::write(&dst, custom).unwrap();
        let reason = foreign_destination_reason_with(&dst, "code-reviewer.md", None, &[], &[])
            .expect("an unknown file is kept");
        assert!(
            matches!(&reason, SkipReason::OperatorAuthored(why) if why.contains("operator-authored")),
            "{reason:?}"
        );

        let line = format!(
            "code-reviewer.md {}",
            crate::provenance::git_blob_id(custom)
        );
        assert_eq!(
            foreign_destination_reason_with(&dst, "code-reviewer.md", None, &[], &[&line]),
            None,
            "a shipped blob is refreshed"
        );
        std::fs::write(
            &dst,
            b"---\r\nname: code-reviewer\r\n---\r\nmy own reviewer\r\n",
        )
        .unwrap();
        assert_eq!(
            foreign_destination_reason_with(&dst, "code-reviewer.md", None, &[], &[&line]),
            None,
            "an autocrlf copy of a shipped blob is still ours"
        );
        let bundled = FLEET_AGENTS
            .get_file("code-reviewer.md")
            .expect("bundled")
            .contents();
        std::fs::write(&dst, bundled).unwrap();
        assert_eq!(
            foreign_destination_reason(&dst, "code-reviewer.md", None, &[]),
            None,
            "the bundled body itself is ours"
        );
    }

    /// The current overlay source's bytes are ours too: they are what the
    /// overlay would copy.
    #[test]
    fn an_exact_copy_of_the_overlay_source_is_ours() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("local-agent.md"), b"# overlay body\n").unwrap();
        let dst = tmp.path().join("local-agent.md");
        std::fs::write(&dst, b"# overlay body\n").unwrap();
        assert_eq!(
            foreign_destination_reason(&dst, "local-agent.md", Some(&src), &[]),
            None
        );
        std::fs::write(&dst, b"# an operator edit\n").unwrap();
        assert!(foreign_destination_reason(&dst, "local-agent.md", Some(&src), &[]).is_some());
    }

    /// The floor writer never writes THROUGH a destination symlink, even with
    /// nothing kept: the file the link points at keeps its bytes, and the name
    /// is skipped with a reason.
    #[cfg(unix)]
    #[test]
    fn the_floor_never_writes_through_a_destination_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("operator-source.md");
        std::fs::write(&target, b"OPERATOR SOURCE").unwrap();
        let dst = tmp.path().join(".claude").join("agents");
        std::fs::create_dir_all(&dst).unwrap();
        std::os::unix::fs::symlink(&target, dst.join("code-reviewer.md")).unwrap();

        let report = provision_fleet_agents_into(&dst, &KeepDecisions::default()).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"OPERATOR SOURCE");
        assert!(std::fs::symlink_metadata(dst.join("code-reviewer.md"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(
            report.skipped.iter().any(|u| u.unit == "code-reviewer.md"
                && matches!(&u.reason, SkipReason::OperatorAuthored(why) if why.contains("symbolic link"))),
            "{:?}",
            report.skipped
        );
        assert_eq!(report.written, embedded_agent_count() - 1);
        assert!(foreign_destination_reason(
            &dst.join("code-reviewer.md"),
            "code-reviewer.md",
            None,
            &[]
        )
        .is_some());
    }

    #[test]
    fn every_embedded_agent_def_declares_a_name() {
        // `claude` resolves a subagent by the `name:` in the definition's YAML
        // frontmatter, NOT by filename. A def whose frontmatter is missing or
        // unnamed is provisioned but unresolvable — the same silent-no-subagent
        // failure this module exists to remove, just moved one step later.
        let mut checked = 0usize;
        for file in FLEET_AGENTS.files() {
            let Some(text) = file.contents_utf8() else {
                continue;
            };
            let path = file.path().display();
            assert!(
                text.starts_with("---"),
                "bundled agent def {path} has no YAML frontmatter — `claude` cannot \
                 resolve it; add one in src-tauri/src/fleet_agents/"
            );
            let front = text.split("---").nth(1).unwrap_or_default();
            assert!(
                front.lines().any(|l| l.trim_start().starts_with("name:")),
                "bundled agent def {path} declares no `name:` in its frontmatter — \
                 `claude` resolves a subagent by that field, not by filename, so this \
                 def ships unresolvable"
            );
            checked += 1;
        }
        assert!(
            checked > 0,
            "no agent defs scanned — either the bundle is empty or the include_dir! \
             root went stale"
        );
    }

    #[test]
    fn bundled_agent_defs_have_no_operator_local_paths() {
        // These bodies ship to every fleet device, so a path rooted on one
        // operator's machine is a dead pointer everywhere else. Mirrors
        // `fleet_skills::tests::bundled_skills_have_no_operator_local_paths`.
        const FORBIDDEN: &[&str] = &[
            "D:/qontinui-root",
            "D:\\qontinui-root",
            "C:/Users/",
            "/home/",
        ];
        let mut checked = 0usize;
        for file in FLEET_AGENTS.files() {
            let Some(text) = file.contents_utf8() else {
                continue;
            };
            for pat in FORBIDDEN {
                assert!(
                    !text.contains(pat),
                    "bundled agent def {} contains operator-local path {pat:?} — it ships \
                     to every fleet device, where that path does not exist",
                    file.path().display()
                );
            }
            checked += 1;
        }
        assert!(checked > 0, "no agent defs scanned — guard went stale");
    }

    #[test]
    fn the_policy_named_reviewer_is_bundled() {
        // `code-review-invocation-path` requires the `code-reviewer` subagent for
        // every pre-PR review and forbids `/code-review` (operator-only). If this
        // def is not in the bundle, that policy is unsatisfiable on any device
        // without a claude-config checkout — which is the case this module fixes.
        assert!(
            FLEET_AGENTS.get_file("code-reviewer.md").is_some(),
            "code-reviewer.md is not bundled — the fleet's pre-PR-review policy names \
             this subagent specifically, so dropping it from the bundle silently makes \
             that policy unsatisfiable on a checkout-less device"
        );
    }

    /// The `test-generator.md` line is pinned byte for byte, because a blob
    /// dropped from it would leave that version's copies on disk forever. The
    /// qontinui-claude-config PR that deletes the source will give
    /// `scripts/install-agents.sh` the same line.
    #[test]
    fn the_retired_table_carries_every_blob_test_generator_shipped_as() {
        assert_eq!(
            RETIRED_AGENTS,
            &[
                "test-generator.md 3908f6b9811fea0b8f7bc68f7d0bf9022015a537 \
               d80c5d1d620bb8f9e2acdd756eadc22b624794a0 \
               8c23f14a7cbaaf04d35f68ad13745242f91012b6 \
               d8d6673b504bb9f37155ec0acecb8fc02859ae95 \
               7b5318b48ebfaaeaba1aaff7d86f99c0bbb07535 \
               0bb59a9cea7f0a8c598f648e40f27300bcda9dde \
               7b6360157dc99a9c86919343a0cc3a65b8ee6091 \
               565d8136ded8048c22b8ba3c5b4e771a1f6e2aac"
            ]
        );
        assert_eq!(
            retired_agent_names(RETIRED_AGENTS),
            vec!["test-generator.md".to_string()]
        );
    }

    /// Every line parses into a top-level `.md` name and at least one full,
    /// lowercase blob id, and no retired name is still bundled: a bundled one
    /// would be written by the floor and removed again on every pass.
    #[test]
    fn every_retired_line_is_well_formed_and_no_longer_bundled() {
        assert!(!RETIRED_AGENTS.is_empty(), "the table guard went stale");
        let bundled = embedded_agent_names();
        for line in RETIRED_AGENTS {
            let (name, blobs) = parse_retired_line(line);
            assert!(
                name.ends_with(".md") && !name.contains('/') && !name.contains('\\'),
                "retired name {name:?} is not a top-level definition file"
            );
            assert!(
                !blobs.is_empty(),
                "{name}: a line with no blob removes nothing"
            );
            for blob in &blobs {
                assert!(
                    blob.len() == 40
                        && blob
                            .chars()
                            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                    "{name}: {blob:?} is not a full lowercase git blob id"
                );
            }
            assert!(
                !bundled.iter().any(|b| b == name),
                "{name} is retired but still bundled in src-tauri/src/fleet_agents/"
            );
        }
    }

    /// A blob matches on the bytes as they are and on the CRLF-folded bytes
    /// (an autocrlf checkout's copy of an LF blob), and on nothing else. A lone
    /// CR is content, not a line ending, so folding keeps it.
    #[test]
    fn a_shipped_blob_matches_as_written_or_with_crlf_folded_and_nothing_else() {
        let lf = b"---\nname: x\n---\nbody\n".as_slice();
        let lf_blob = crate::provenance::git_blob_id(lf);
        let shipped = [lf_blob.as_str()];
        assert_eq!(shipped_blob(lf, &shipped), Ok(lf_blob.clone()));
        let crlf = b"---\r\nname: x\r\n---\r\nbody\r\n".as_slice();
        assert_eq!(
            shipped_blob(crlf, &shipped),
            Ok(lf_blob.clone()),
            "an autocrlf copy of a shipped LF blob is the shipped content"
        );
        let edited = b"---\nname: x\n---\nedited\n".as_slice();
        assert_eq!(
            shipped_blob(edited, &shipped),
            Err(crate::provenance::git_blob_id(edited)),
            "an edited copy reports its own blob"
        );
        assert_eq!(
            shipped_blob(crlf, &[]),
            Err(crate::provenance::git_blob_id(crlf)),
            "an unmatched CRLF copy reports its raw blob, not the folded one"
        );
        assert_eq!(fold_crlf(b"a\r\nb\rc\r\n"), b"a\nb\rc\n".to_vec());
    }

    /// Run `git` in `dir` with a throwaway identity, panicking on failure.
    fn git(dir: &Path, args: &[&str]) {
        // Scrubbed of the repo-local git environment, like
        // `provision_guard::test_support`: a run from inside a git hook would
        // otherwise act on the hook's repository.
        let out = crate::process_helpers::no_window("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A blob counts as the overlay checkout's own only when THIS definition's
    /// path held it. The same bytes committed at another path in the same
    /// repository (another definition) are not evidence the destination is a
    /// copy of this one, so the operator's file is kept; a former version of
    /// this definition's own path is ours and may be refreshed.
    #[test]
    fn a_blob_held_only_at_a_different_path_is_not_ours() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("ccfg");
        let src = repo.join(".claude").join("agents");
        std::fs::create_dir_all(&src).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(src.join("other-agent.md"), b"# shared bytes\n").unwrap();
        std::fs::write(src.join("local-agent.md"), b"# version one\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        std::fs::write(src.join("local-agent.md"), b"# version two\n").unwrap();
        git(&repo, &["commit", "-q", "-am", "two"]);

        let dst_dir = tmp.path().join("dst");
        std::fs::create_dir_all(&dst_dir).unwrap();
        let dst = dst_dir.join("local-agent.md");

        std::fs::write(&dst, b"# shared bytes\n").unwrap();
        let reason = foreign_destination_reason_with(&dst, "local-agent.md", Some(&src), &[], &[])
            .expect("bytes held only at another path are kept");
        assert!(
            matches!(&reason, SkipReason::OperatorAuthored(why) if why.contains("operator-authored")),
            "{reason:?}"
        );

        std::fs::write(&dst, b"# version one\n").unwrap();
        assert_eq!(
            foreign_destination_reason_with(&dst, "local-agent.md", Some(&src), &[], &[]),
            None,
            "a former version of this definition's own path is ours"
        );
        std::fs::write(&dst, b"# version one\r\n").unwrap();
        assert_eq!(
            foreign_destination_reason_with(&dst, "local-agent.md", Some(&src), &[], &[]),
            None,
            "an autocrlf copy of a former version is ours too"
        );
    }

    /// A probe that cannot answer (here: `git -C` on a directory that does not
    /// exist, which exits 128) is UNKNOWN, not "no". The file is kept, and the
    /// recorded detail says the probe did not answer rather than claiming the
    /// blob is not one this runner ships or shipped.
    #[test]
    fn an_unanswered_ownership_probe_keeps_the_file_as_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp
            .path()
            .join("no-such-checkout")
            .join(".claude")
            .join("agents");
        let probe = checkout_history_holds_blob(&missing, "local-agent.md", &["a".repeat(40)]);
        assert!(
            matches!(&probe, HistoryProbe::NoAnswer { why } if why.starts_with("exit ")),
            "{probe:?}"
        );

        let dst = tmp.path().join("local-agent.md");
        std::fs::write(&dst, b"# operator bytes\n").unwrap();
        let reason =
            foreign_destination_reason_with(&dst, "local-agent.md", Some(&missing), &[], &[])
                .expect("an unanswered probe keeps the file");
        let SkipReason::OperatorAuthored(why) = &reason else {
            panic!("{reason:?}");
        };
        assert!(
            why.starts_with("ownership probe did not answer (exit "),
            "{why}"
        );
        assert!(!why.contains("ships or shipped"), "{why}");
    }

    /// A clean probe whose history lacks the blob is a definite "no", recorded
    /// with the definite wording.
    #[test]
    fn an_answered_probe_without_the_blob_is_a_definite_no() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("ccfg");
        let src = repo.join(".claude").join("agents");
        std::fs::create_dir_all(&src).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(src.join("local-agent.md"), b"# version one\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        assert_eq!(
            checkout_history_holds_blob(&src, "local-agent.md", &["a".repeat(40)]),
            HistoryProbe::No
        );
    }

    /// The raw-log parser matches a blob on either side of a change, and
    /// nothing in a path or a status.
    #[test]
    fn the_raw_log_parser_reads_both_sides_of_a_change_and_nothing_else() {
        let old = "a".repeat(40);
        let new = "b".repeat(40);
        let line = format!(":100644 100644 {old} {new} M\tx.md\n");
        assert!(raw_log_names_blob(&line, &[old.clone()]));
        assert!(raw_log_names_blob(&line, &[new.clone()]));
        assert!(!raw_log_names_blob(&line, &["c".repeat(40)]));
        let path_only = format!(":100644 100644 {old} {old} M\t{new}\n");
        assert!(!raw_log_names_blob(&path_only, &[new]));
        assert!(!raw_log_names_blob("", &[old]));
    }

    /// An atomic write replaces a destination hard link as a directory entry:
    /// the file the link shared its inode with keeps its bytes.
    #[cfg(unix)]
    #[test]
    fn an_atomic_write_replaces_a_hard_link_instead_of_writing_through_it() {
        let tmp = tempfile::tempdir().unwrap();
        let other = tmp.path().join("operator.md");
        std::fs::write(&other, b"OPERATOR").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o640)).unwrap();
        let dst = tmp.path().join("code-reviewer.md");
        std::fs::hard_link(&other, &dst).unwrap();

        write_atomically(&dst, b"NEW", None).unwrap();

        assert_eq!(std::fs::read(&other).unwrap(), b"OPERATOR");
        assert_eq!(std::fs::read(&dst).unwrap(), b"NEW");
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temporary file is left behind");
        assert_eq!(
            std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777,
            0o640,
            "with no permissions given, the replaced destination's mode is kept"
        );
    }

    /// With no permissions given, a NEW destination is `0644` (not the `0600`
    /// a temporary file is created with), and an existing one keeps its mode.
    #[cfg(unix)]
    #[test]
    fn an_atomic_write_without_permissions_preserves_an_existing_mode_else_0644() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;

        let fresh = tmp.path().join("fresh.md");
        write_atomically(&fresh, b"NEW", None).unwrap();
        assert_eq!(mode(&fresh), 0o644);

        let existing = tmp.path().join("existing.md");
        std::fs::write(&existing, b"OLD").unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
        write_atomically(&existing, b"NEW", None).unwrap();
        assert_eq!(mode(&existing), 0o755);
        assert_eq!(std::fs::read(&existing).unwrap(), b"NEW");

        // Explicit permissions still win over the destination's.
        write_atomically(
            &existing,
            b"NEWER",
            Some(std::fs::Permissions::from_mode(0o600)),
        )
        .unwrap();
        assert_eq!(mode(&existing), 0o600);
    }

    /// A write that cannot land leaves no temporary file behind.
    #[test]
    fn a_failed_atomic_write_leaves_no_temporary_file() {
        let tmp = tempfile::tempdir().unwrap();
        // The destination is a non-empty directory, so the rename fails.
        let dst = tmp.path().join("code-reviewer.md");
        std::fs::create_dir_all(dst.join("inner")).unwrap();
        assert!(write_atomically(&dst, b"NEW", None).is_err());
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
}
