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
//! they have today, byte for byte; a device with no checkout gets the embedded
//! set instead of nothing.
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

use crate::capability_manifest::{self, ProvisionReport, SkipReason};
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
/// overwrites existing files (idempotent) EXCEPT those `keep` names: a
/// definition the enclosing git repository tracks (or one that exists when the
/// tracked-file probe could not answer inside a repository) is left alone and
/// reported as skipped, exactly as [`crate::fleet_commands`] and
/// [`crate::fleet_skills`] do. In a `qontinui-claude-config` checkout
/// `.claude/agents/*.md` IS tracked content, and overwriting it with this
/// binary's render dirtied every such worktree.
///
/// Only `*.md` at the top level is written — the same filter the checkout copy
/// applies, so the two layers cannot disagree about what counts as a definition.
///
/// It removes nothing. Removing a RETIRED definition is
/// [`remove_retired_agents`]'s job, which the caller runs after the checkout
/// overlay, so a copy a stale overlay rewrites is removed too.
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
        // Per-file rather than `?`: one unwritable definition must not cost the
        // session the other four, and it must be NAMED rather than aborting the
        // pass at whatever point it happened to reach.
        match std::fs::write(dst_dir.join(name), file.contents()) {
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
/// separated by single spaces. That is the shape of the `RETIRED_AGENTS` table
/// in `qontinui-claude-config/scripts/install-agents.sh`, the installer's half
/// of the same retirement, and a file's line is identical in both.
///
/// A line lists the blobs of the SOURCE's whole history, not only the ones this
/// directory bundled, because the checkout overlay copies from the source and a
/// workdir can hold any of them. A line only grows: the bundle sync never
/// re-adds a deleted file, so a blob that ships after the line is written is
/// never learned here.
pub(crate) const RETIRED_AGENTS: &[&str] = &[
    // Retired by plan
    // 2026-11-06-what-the-fleet-records-and-advertises-matches-what-actually-ran,
    // Phase 6: nothing dispatched it, and its body contradicted served policy
    // (`testing` coverage-is-enumerated-not-salient and a-test-must-be-able-to-fail;
    // `escalation-bar` escalation-closed-list).
    // The first five blobs are its whole history in qontinui-claude-config, of
    // which this directory shipped 8c23f14a, d80c5d1d and 3908f6b9. The sixth is
    // its blob at the head (4a60b2d4f3d4) of qontinui-claude-config#2413, which
    // was open and editing it when it was retired.
    "test-generator.md 3908f6b9811fea0b8f7bc68f7d0bf9022015a537 d80c5d1d620bb8f9e2acdd756eadc22b624794a0 8c23f14a7cbaaf04d35f68ad13745242f91012b6 d8d6673b504bb9f37155ec0acecb8fc02859ae95 7b5318b48ebfaaeaba1aaff7d86f99c0bbb07535 7b6360157dc99a9c86919343a0cc3a65b8ee6091",
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

/// Remove from `dst_dir` every definition `retired` lists whose copy on disk is
/// a blob it shipped as, and record each removal on `report`.
///
/// What stays, by arm:
/// - **absent**: nothing to do;
/// - **kept by `keep`**: a copy the enclosing repository tracks, or one that
///   existed when the tracked-file probe could not answer inside a repository,
///   stays exactly as both provisioning layers keep it (`keep` was decided
///   before either layer wrote);
/// - **any other blob**: somebody edited it, so it is kept and logged as
///   locally modified. That is the own-files-only rule `install-agents.sh
///   --uninstall` applies;
/// - **unreadable or unremovable**: kept, and recorded on `report` as a
///   [`SkipReason::WriteFailed`] skip, so the pass reads degraded instead of
///   clean.
///
/// A blob is matched on the bytes as they are and on the same bytes with CRLF
/// folded to LF. A checkout with `core.autocrlf=true` holds a CRLF copy of a
/// blob git stores as LF, and that copy is the shipped content, not an edit
/// (`agent-bundle-parity.sh` compares through the same fold).
///
/// It never fails and never panics: a pass that could not finish still leaves
/// the spawn running, as with both layers before it.
pub(crate) fn remove_retired_agents(
    dst_dir: &Path,
    keep: &KeepDecisions,
    retired: &[&str],
    report: &mut ProvisionReport,
) {
    for line in retired {
        let (name, shipped) = parse_retired_line(line);
        let path = dst_dir.join(name);
        if let Some(reason) = keep.reason(name) {
            tracing::info!(
                "fleet_agents: keeping retired {} as it is on disk — {}",
                path.display(),
                reason.describe()
            );
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
                report.skip(
                    name,
                    SkipReason::WriteFailed(format!(
                        "read retired definition to compare its blob: {e}"
                    )),
                );
                continue;
            }
        };
        let Some(blob) = shipped_blob(&bytes, &shipped) else {
            tracing::warn!(
                "fleet_agents: keeping retired {} — locally modified (blob {} is not one it shipped as)",
                path.display(),
                crate::provenance::git_blob_id(&bytes)
            );
            continue;
        };
        match std::fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!(
                    "fleet_agents: removed retired {} (blob {blob}, one it shipped as)",
                    path.display()
                );
                report.record_removed(name, blob);
            }
            Err(e) => {
                tracing::warn!(
                    "fleet_agents: could not remove retired {}: {e}",
                    path.display()
                );
                report.skip(
                    name,
                    SkipReason::WriteFailed(format!("remove retired definition: {e}")),
                );
            }
        }
    }
}

/// The blob in `shipped` that `bytes` is, as they are or with CRLF folded to LF.
fn shipped_blob(bytes: &[u8], shipped: &[&str]) -> Option<String> {
    let raw = crate::provenance::git_blob_id(bytes);
    if shipped.contains(&raw.as_str()) {
        return Some(raw);
    }
    if !bytes.windows(2).any(|pair| pair == b"\r\n") {
        return None;
    }
    let folded = crate::provenance::git_blob_id(&fold_crlf(bytes));
    shipped.contains(&folded.as_str()).then_some(folded)
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

    /// The `test-generator.md` line is pinned byte for byte: it must equal the
    /// line `qontinui-claude-config/scripts/install-agents.sh` carries, and a
    /// blob dropped from it would leave that version's copies on disk forever.
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
               7b6360157dc99a9c86919343a0cc3a65b8ee6091"
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
        assert_eq!(shipped_blob(lf, &shipped), Some(lf_blob.clone()));
        assert_eq!(
            shipped_blob(b"---\r\nname: x\r\n---\r\nbody\r\n", &shipped),
            Some(lf_blob.clone()),
            "an autocrlf copy of a shipped LF blob is the shipped content"
        );
        assert_eq!(shipped_blob(b"---\nname: x\n---\nedited\n", &shipped), None);
        assert_eq!(shipped_blob(lf, &[]), None);
        assert_eq!(fold_crlf(b"a\r\nb\rc\r\n"), b"a\nb\rc\n".to_vec());
    }
}
