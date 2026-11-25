//! A promoted build's machine-readable provenance, `PROVENANCE.json`, and the
//! binary's own `--provenance` answer.
//!
//! Two different witnesses, on purpose (plan
//! `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`,
//! D3.1):
//!
//! - **`PROVENANCE.json`** sits beside the promoted binaries in
//!   `target/primary/<sha>/` and records what the PROMOTER saw: the sha, the
//!   build id, the sibling pins it built against, and the sha256 of every file
//!   it promoted. Verifying the files against it proves nothing was swapped
//!   or truncated after promotion.
//! - **`qontinui-runner --provenance`** is the binary's own compile-time
//!   identity (`gitSha`, `gitDirty`, `buildId`, `buildProfile`). The dirty bit
//!   is read ONLY from here — a sidecar cannot vouch for the tree a binary was
//!   compiled from; the binary can, and cannot lie about itself.
//!
//! The writer ([`write_promoted_provenance`]) is the one Phase 6's
//! `build-latest` task calls when it promotes a build; it lives here so the
//! reader and the writer are one format.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The file name inside a promoted directory.
pub const PROVENANCE_FILE: &str = "PROVENANCE.json";

/// The format version this build reads and writes.
pub const PROVENANCE_SCHEMA: u32 = 1;

/// The binaries a promoted directory must list: the runner itself, and the
/// holder whose protocol list the preflight reads.
pub const REQUIRED_BINARIES: &[&str] = &[RUNNER_BINARY, HOLDER_BINARY];

#[cfg(windows)]
pub const RUNNER_BINARY: &str = "qontinui-runner.exe";
#[cfg(not(windows))]
pub const RUNNER_BINARY: &str = "qontinui-runner";
#[cfg(windows)]
pub const HOLDER_BINARY: &str = "qontinui-pty-holder.exe";
#[cfg(not(windows))]
pub const HOLDER_BINARY: &str = "qontinui-pty-holder";

/// `PROVENANCE.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromotedProvenance {
    pub schema: u32,
    /// The full commit sha the build was made from.
    pub sha: String,
    /// The `/health` `buildId` the build reports.
    pub build_id: String,
    /// The cargo profile directory it was built into, when known.
    pub build_profile: Option<String>,
    /// Sibling repo → the sha this build compiled against.
    pub sibling_pins: BTreeMap<String, String>,
    /// Promoted file name → lowercase hex sha256.
    pub files: BTreeMap<String, String>,
    /// RFC 3339, when the promotion wrote this file.
    pub promoted_at: String,
}

/// Why a `PROVENANCE.json` could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvenanceError {
    /// No such file — a directory promoted before this format existed (its
    /// prose `PROVENANCE.txt` carries no dirty bit and is not read).
    Missing,
    Unreadable(String),
    Malformed(String),
    UnsupportedSchema(u32),
}

impl std::fmt::Display for ProvenanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "{PROVENANCE_FILE} is missing"),
            Self::Unreadable(e) => write!(f, "{PROVENANCE_FILE} could not be read: {e}"),
            Self::Malformed(e) => write!(f, "{PROVENANCE_FILE} is malformed: {e}"),
            Self::UnsupportedSchema(s) => write!(
                f,
                "{PROVENANCE_FILE} schema {s} is not one this build reads ({PROVENANCE_SCHEMA})"
            ),
        }
    }
}

/// Lowercase hex sha256 of a file's bytes.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(buf.get(..n).unwrap_or(&[]));
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Read `<dir>/PROVENANCE.json`.
pub fn read_promoted_provenance(dir: &Path) -> Result<PromotedProvenance, ProvenanceError> {
    let path = dir.join(PROVENANCE_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(ProvenanceError::Missing),
        Err(e) => return Err(ProvenanceError::Unreadable(e.to_string())),
    };
    let p: PromotedProvenance =
        serde_json::from_str(&text).map_err(|e| ProvenanceError::Malformed(e.to_string()))?;
    if p.schema != PROVENANCE_SCHEMA {
        return Err(ProvenanceError::UnsupportedSchema(p.schema));
    }
    Ok(p)
}

/// What checking a promoted directory's files against its provenance found.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileVerification {
    /// Listed and hashing to the recorded digest.
    pub matched: Vec<String>,
    /// Listed but hashing to something else: `(name, recorded, actual)`.
    pub mismatched: Vec<(String, String, String)>,
    /// Listed but absent or unreadable: `(name, error)`.
    pub unreadable: Vec<(String, String)>,
    /// A [`REQUIRED_BINARIES`] entry the provenance does not list.
    pub unlisted_required: Vec<String>,
    /// A listed name that is not a plain file name (a path, `..`).
    pub invalid_names: Vec<String>,
}

impl FileVerification {
    pub fn ok(&self) -> bool {
        self.mismatched.is_empty()
            && self.unreadable.is_empty()
            && self.unlisted_required.is_empty()
            && self.invalid_names.is_empty()
            && !self.matched.is_empty()
    }

    pub fn describe(&self) -> String {
        let mut parts = vec![format!("{} file(s) match", self.matched.len())];
        for (n, want, got) in &self.mismatched {
            parts.push(format!("{n} hashes to {got}, PROVENANCE.json says {want}"));
        }
        for (n, e) in &self.unreadable {
            parts.push(format!("{n} unreadable ({e})"));
        }
        for n in &self.unlisted_required {
            parts.push(format!("{n} not listed"));
        }
        for n in &self.invalid_names {
            parts.push(format!("listed name {n:?} is not a plain file name"));
        }
        parts.join("; ")
    }
}

fn plain_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

/// Hash every file the provenance lists and compare.
pub fn verify_promoted_files(dir: &Path, p: &PromotedProvenance) -> FileVerification {
    let mut v = FileVerification::default();
    for (name, want) in &p.files {
        if !plain_file_name(name) {
            v.invalid_names.push(name.clone());
            continue;
        }
        match sha256_file(&dir.join(name)) {
            Ok(got) if got.eq_ignore_ascii_case(want) => v.matched.push(name.clone()),
            Ok(got) => v.mismatched.push((name.clone(), want.clone(), got)),
            Err(e) => v.unreadable.push((name.clone(), e.to_string())),
        }
    }
    for req in REQUIRED_BINARIES {
        if !p.files.contains_key(*req) {
            v.unlisted_required.push((*req).to_string());
        }
    }
    v
}

/// What a promotion knows when it writes `PROVENANCE.json`.
#[derive(Debug, Clone)]
pub struct PromotionFacts {
    pub sha: String,
    pub build_id: String,
    pub build_profile: Option<String>,
    pub sibling_pins: BTreeMap<String, String>,
    /// The promoted files to hash, by name, inside the promoted directory.
    pub files: Vec<String>,
}

/// Write `<dir>/PROVENANCE.json` for files already copied into `dir`, hashing
/// each one. Atomic (temp file + rename), so a reader never sees half a file.
/// The writer Phase 6's `build-latest` promotion calls.
pub fn write_promoted_provenance(
    dir: &Path,
    facts: &PromotionFacts,
    promoted_at: &str,
) -> io::Result<PromotedProvenance> {
    let mut files = BTreeMap::new();
    for name in &facts.files {
        if !plain_file_name(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name:?} is not a plain file name"),
            ));
        }
        files.insert(name.clone(), sha256_file(&dir.join(name))?);
    }
    let p = PromotedProvenance {
        schema: PROVENANCE_SCHEMA,
        sha: facts.sha.clone(),
        build_id: facts.build_id.clone(),
        build_profile: facts.build_profile.clone(),
        sibling_pins: facts.sibling_pins.clone(),
        files,
        promoted_at: promoted_at.to_string(),
    };
    let text = serde_json::to_string_pretty(&p).map_err(io::Error::other)?;
    super::record::write_atomic(&dir.join(PROVENANCE_FILE), text.as_bytes())?;
    Ok(p)
}

/// A binary's own `--provenance` answer
/// (`qontinui_runner_lib::build_provenance::self_provenance_json`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BinaryProvenance {
    pub git_sha: String,
    /// `None` = the build could not measure it: UNKNOWN, never clean.
    pub git_dirty: Option<bool>,
    pub build_id: String,
    pub build_profile: Option<String>,
}

/// Parse a `--provenance` stdout (one JSON object).
pub fn parse_binary_provenance(stdout: &str) -> Result<BinaryProvenance, String> {
    serde_json::from_str(stdout.trim()).map_err(|e| format!("unreadable --provenance answer: {e}"))
}

/// The runner binary inside a promoted directory.
pub fn runner_binary_in(dir: &Path) -> PathBuf {
    dir.join(RUNNER_BINARY)
}

/// The holder binary inside a promoted directory.
pub fn holder_binary_in(dir: &Path) -> PathBuf {
    dir.join(HOLDER_BINARY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn promote_fixture(dir: &Path, sha: &str) -> PromotedProvenance {
        std::fs::write(dir.join(RUNNER_BINARY), b"runner bytes").unwrap();
        std::fs::write(dir.join(HOLDER_BINARY), b"holder bytes").unwrap();
        let facts = PromotionFacts {
            sha: sha.to_string(),
            build_id: format!("{}-1", sha.get(..9).unwrap_or(sha)),
            build_profile: Some("release-checked".into()),
            sibling_pins: BTreeMap::from([(
                "qontinui-schemas".to_string(),
                "a7b273d8".to_string(),
            )]),
            files: vec![RUNNER_BINARY.to_string(), HOLDER_BINARY.to_string()],
        };
        write_promoted_provenance(dir, &facts, "2026-11-24T00:00:00Z").unwrap()
    }

    #[test]
    fn written_provenance_reads_back_and_verifies() {
        let d = tempfile::tempdir().unwrap();
        let written = promote_fixture(d.path(), "4d524e71bcc3c0ed4d9ef4b860c6ddd229f6696a");
        let read = read_promoted_provenance(d.path()).unwrap();
        assert_eq!(read, written);
        let v = verify_promoted_files(d.path(), &read);
        assert!(v.ok(), "{}", v.describe());
        assert_eq!(v.matched.len(), 2);
    }

    #[test]
    fn a_changed_file_fails_verification() {
        let d = tempfile::tempdir().unwrap();
        let p = promote_fixture(d.path(), "4d524e71bcc3c0ed4d9ef4b860c6ddd229f6696a");
        std::fs::write(d.path().join(RUNNER_BINARY), b"swapped after promotion").unwrap();
        let v = verify_promoted_files(d.path(), &p);
        assert!(!v.ok());
        assert_eq!(v.mismatched.len(), 1);
        assert!(v.describe().contains("hashes to"));
    }

    #[test]
    fn missing_malformed_and_foreign_schema_are_distinct() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            read_promoted_provenance(d.path()),
            Err(ProvenanceError::Missing)
        );
        std::fs::write(d.path().join(PROVENANCE_FILE), "not json").unwrap();
        assert!(matches!(
            read_promoted_provenance(d.path()),
            Err(ProvenanceError::Malformed(_))
        ));
        let mut p = promote_fixture(d.path(), "4d524e71bcc3c0ed4d9ef4b860c6ddd229f6696a");
        p.schema = 9;
        std::fs::write(
            d.path().join(PROVENANCE_FILE),
            serde_json::to_string(&p).unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_promoted_provenance(d.path()),
            Err(ProvenanceError::UnsupportedSchema(9))
        );
    }

    #[test]
    fn a_path_shaped_name_or_an_unlisted_runner_fails() {
        let d = tempfile::tempdir().unwrap();
        let mut p = promote_fixture(d.path(), "4d524e71bcc3c0ed4d9ef4b860c6ddd229f6696a");
        p.files.remove(RUNNER_BINARY);
        p.files.insert("../escape".into(), "00".into());
        let v = verify_promoted_files(d.path(), &p);
        assert!(!v.ok());
        assert_eq!(v.unlisted_required, vec![RUNNER_BINARY.to_string()]);
        assert_eq!(v.invalid_names, vec!["../escape".to_string()]);
    }

    #[test]
    fn binary_provenance_parses_the_self_provenance_object() {
        let live = qontinui_runner_lib::build_provenance::self_provenance_json();
        let parsed = parse_binary_provenance(&live.to_string()).unwrap();
        assert_eq!(parsed.git_sha, env!("QONTINUI_GIT_SHA"));
        let dirty = parse_binary_provenance(
            r#"{"gitSha":"abc1234","gitDirty":null,"buildId":"x","buildProfile":null}"#,
        )
        .unwrap();
        assert_eq!(dirty.git_dirty, None);
        assert!(parse_binary_provenance("usage").is_err());
    }
}
