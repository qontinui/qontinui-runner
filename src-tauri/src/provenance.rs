//! The `qontinui-provenance:` frontmatter key every provisioned markdown file
//! carries — ONE implementation, shared by both fleet provisioners.
//!
//! A provisioned command body (`crate::fleet_commands`) and a provisioned
//! `SKILL.md` (`crate::fleet_skills`) are written with one generated key at
//! line 2 of their YAML frontmatter:
//!
//! ```text
//! qontinui-provenance: source=<builtin|served|disk_cache|canonical> canonical=qontinui-claude-config:<path> blob=<sha1> runner_build=<RUNNER_BUILD_ID> [canonical_sha=<sha12>]
//! ```
//!
//! `canonical_sha` is present exactly when `source=canonical`: the body was read
//! out of the runner's mirror of `qontinui-claude-config` (`crate::canonical_corpus`)
//! at that `origin/main` commit, so `git -C qontinui-claude-config show
//! <canonical_sha>:<path> | git hash-object --stdin` equals `blob`. Every other
//! source omits it, so a pre-canonical file and a builtin one read identically.
//!
//! `canonical` names the file in `qontinui-claude-config` the body is a copy
//! of — `.claude/commands/<name>.md` for a command ([`command_canonical`]),
//! `.claude/skills/<name>/SKILL.md` for a skill ([`skill_canonical`]). It is
//! the only thing that differs between the two callers, so it is the one
//! parameter; the grammar, the hash rule and the strip rule are shared. Two
//! copies of a hash rule diverge, and a comparator would then need two strip
//! rules to read one tree.
//!
//! `blob` is the git blob id of the body bytes with the key excluded. For a
//! builtin it equals `git hash-object` of the VENDORED file this build
//! embedded — and of the canonical file only while the two are in byte parity.
//! After a fetch, `git -C qontinui-claude-config log --all --find-object=<blob>
//! -- <path>` separates a stale copy (the blob is an older canonical version)
//! from a fork (no version of that file ever held it).
//!
//! A provisioned file is checkable on its own — no sibling checkout, no runner —
//! by [`provenance_consistent`], or from a shell: when line 3 is `---` (a
//! prepended block) `tail -n +4 <file> | git hash-object --stdin`, otherwise
//! (the key was inserted into existing frontmatter) `sed 2d <file> | git
//! hash-object --stdin`; with no git at all, the sha1 of
//! `blob <byte-length>\0<body>`. The key is a write-time transform only: the
//! embedded sources and the files beside the provisioners never carry it, so a
//! stamped file inside a git-tracked `.claude/` is a provisioner overwrite
//! proven by the file itself.
//!
//! Its Python twin is `qontinui-claude-config/scripts/lint-command-frontmatter.py`
//! — a grammar change here is a change there, in the same PR pair.

/// The YAML frontmatter key [`with_provenance`] writes at line 2 of every
/// provisioned markdown file.
pub(crate) const PROVENANCE_KEY: &str = "qontinui-provenance:";

/// The build identity stamped into `runner_build=` — the same compile-time
/// value `/health` reports as `buildId`.
pub(crate) const RUNNER_BUILD: &str = env!("RUNNER_BUILD_ID");

/// The repository every `canonical=` path is relative to.
const CANONICAL_REPO: &str = "qontinui-claude-config";

/// `canonical=` for a bundled command: `.claude/commands/<name>.md`.
pub(crate) fn command_canonical(name: &str) -> String {
    format!("{CANONICAL_REPO}:.claude/commands/{name}.md")
}

/// `canonical=` for a bundled skill's manifest: `.claude/skills/<name>/SKILL.md`.
pub(crate) fn skill_canonical(name: &str) -> String {
    format!("{CANONICAL_REPO}:.claude/skills/{name}/SKILL.md")
}

/// The parsed fields of one `qontinui-provenance:` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProvenanceLine {
    /// Which rung supplied the body: `builtin` / `served` / `disk_cache` /
    /// `canonical`.
    pub source: String,
    /// `qontinui-claude-config:<path>` — see [`command_canonical`] and
    /// [`skill_canonical`].
    pub canonical: String,
    /// Lowercase 40-hex git blob id of the body, provenance excluded.
    pub blob: String,
    /// The `RUNNER_BUILD_ID` of the binary that wrote the file.
    pub runner_build: String,
    /// The first 12 hex digits of the `qontinui-claude-config` commit a
    /// `source=canonical` body was read at. Optional: absent for every other
    /// source.
    pub canonical_sha: Option<String>,
}

impl ProvenanceLine {
    /// Parse the text after [`PROVENANCE_KEY`]. `None` unless all four
    /// required fields are present; `canonical_sha` is optional.
    fn parse(value: &str) -> Option<Self> {
        let (mut source, mut canonical, mut blob, mut runner_build, mut canonical_sha) =
            (None, None, None, None, None);
        for token in value.split_whitespace() {
            let (k, v) = token.split_once('=')?;
            let slot = match k {
                "source" => &mut source,
                "canonical" => &mut canonical,
                "blob" => &mut blob,
                "runner_build" => &mut runner_build,
                "canonical_sha" => &mut canonical_sha,
                _ => continue,
            };
            *slot = Some(v.to_string());
        }
        Some(Self {
            source: source?,
            canonical: canonical?,
            blob: blob?,
            runner_build: runner_build?,
            canonical_sha,
        })
    }
}

/// Why [`provenance_consistent`] refused a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProvenanceError {
    /// No well-formed `qontinui-provenance:` line at line 2 of a frontmatter
    /// block — the file carries no claim to check.
    Missing,
    /// The body no longer hashes to the blob the line recorded: it was edited
    /// after it was written, or the line was copied onto another body.
    BlobMismatch { recorded: String, actual: String },
}

/// Git blob id (`git hash-object`) of `bytes`, lowercase 40-hex.
pub(crate) fn git_blob_id(bytes: &[u8]) -> String {
    // Hashing an in-memory buffer cannot fail for the Blob type; the fallback
    // keeps this total rather than panicking inside a fail-soft provisioner.
    git2::Oid::hash_object(git2::ObjectType::Blob, bytes)
        .map(|oid| oid.to_string())
        .unwrap_or_default()
}

/// The content of a line with its terminator (`\n` or `\r\n`) removed.
fn line_content(line: &str) -> &str {
    line.strip_suffix('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .unwrap_or(line)
}

/// `text` split into its first line (terminator included) and the remainder.
fn split_first_line(text: &str) -> (&str, &str) {
    match text.find('\n') {
        Some(i) => text.split_at(i + 1),
        None => (text, ""),
    }
}

/// `body` with ONE generated `qontinui-provenance:` key placed at line 2, in
/// YAML frontmatter. `canonical` is the full `qontinui-claude-config:<path>`
/// value ([`command_canonical`] / [`skill_canonical`]); `source` is the rung's
/// wire string; `canonical_sha` is the 12-hex `qontinui-claude-config` commit a
/// `source=canonical` body was read at, and `None` for every other source.
///
/// Placement, because YAML frontmatter is recognised only when it starts at
/// line 1 (the convention Claude Code's command and skill loaders follow) —
/// nothing is ever put above an existing `---`:
/// - a body that opens a NON-EMPTY frontmatter block (`---\n` or `---\r\n`
///   followed by anything but a closing `---`) gets the key inserted as the
///   first line inside it, with the opener's own line ending;
/// - every other body gets a new `---\n<key>\n---\n` block prepended and is
///   otherwise unchanged. That includes a body opening an EMPTY block
///   (`---\n---\n`): inserting into it would produce bytes identical to a
///   prepended block over the empty block's remainder, and
///   [`strip_provenance`] could not tell the two apart.
///
/// `blob` is computed over `body` BEFORE the key is added, so it equals
/// `git hash-object` of the vendored file for an unmodified builtin (and of the
/// canonical file only while the two are in byte parity).
pub(crate) fn with_provenance(
    canonical: &str,
    body: &str,
    source: &str,
    canonical_sha: Option<&str>,
) -> String {
    let mut key = format!(
        "{PROVENANCE_KEY} source={source} canonical={canonical} blob={} \
         runner_build={RUNNER_BUILD}",
        git_blob_id(body.as_bytes()),
    );
    if let Some(sha) = canonical_sha {
        key.push_str(" canonical_sha=");
        key.push_str(sha);
    }
    let (first, rest) = split_first_line(body);
    let opens_block = first == "---\n" || first == "---\r\n";
    let (second, _) = split_first_line(rest);
    if opens_block && line_content(second) != "---" {
        let eol = if first.ends_with("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        format!("{first}{key}{eol}{rest}")
    } else {
        format!("---\n{key}\n---\n{body}")
    }
}

/// Undo [`with_provenance`]: remove exactly the one `qontinui-provenance:` line
/// at line 2 — and the frontmatter block around it when that block is then
/// empty (the one `with_provenance` created) — returning the parsed line and
/// the original body, byte-for-byte. `None` when line 2 is not a well-formed
/// provenance line inside a frontmatter opener.
pub(crate) fn strip_provenance(text: &str) -> Option<(ProvenanceLine, String)> {
    let (first, rest) = split_first_line(text);
    if first != "---\n" && first != "---\r\n" {
        return None;
    }
    let (key_line, after_key) = split_first_line(rest);
    let value = line_content(key_line).strip_prefix(PROVENANCE_KEY)?;
    let parsed = ProvenanceLine::parse(value)?;
    let (third, after_third) = split_first_line(after_key);
    let body = if line_content(third) == "---" {
        // The block holds nothing but the key: `with_provenance` created it.
        after_third.to_string()
    } else {
        format!("{first}{after_key}")
    };
    Some((parsed, body))
}

/// Check a provisioned file against its own provenance line: strip the line,
/// re-hash what remains, and compare with the recorded `blob`. Needs nothing
/// but the file — no sibling checkout, no runner.
pub(crate) fn provenance_consistent(text: &str) -> Result<ProvenanceLine, ProvenanceError> {
    let (line, body) = strip_provenance(text).ok_or(ProvenanceError::Missing)?;
    let actual = git_blob_id(body.as_bytes());
    if actual == line.blob {
        Ok(line)
    } else {
        Err(ProvenanceError::BlobMismatch {
            recorded: line.blob,
            actual,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known answer: `printf 'hello\n' | git hash-object --stdin`.
    #[test]
    fn the_blob_id_is_git_hash_object() {
        assert_eq!(
            git_blob_id(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    /// `strip_provenance` is the exact inverse of `with_provenance` for every
    /// shape — including CRLF frontmatter and the empty-block edge case whose
    /// insertion would be ambiguous — and for both canonical path families.
    #[test]
    fn strip_provenance_round_trips_every_shape() {
        let shapes = [
            "# plain\nbody\n",
            "---\ndescription: x\n---\n# body\n",
            "---\r\ndescription: x\r\nname: y\r\n---\r\n# body\r\n",
            "---\n---\n# empty block\n",
            "---\r\n---\r\n# empty CRLF block\n",
            "---\n",
            "---",
            "",
            "no trailing newline",
        ];
        for canonical in [command_canonical("x"), skill_canonical("x")] {
            for body in shapes {
                for source in ["builtin", "served", "disk_cache"] {
                    let written = with_provenance(&canonical, body, source, None);
                    assert!(
                        written.starts_with("---"),
                        "frontmatter must start at line 1"
                    );
                    let (line, stripped) = strip_provenance(&written)
                        .unwrap_or_else(|| panic!("no provenance found in {written:?}"));
                    assert_eq!(stripped, body, "round trip of {body:?}");
                    assert_eq!(line.source, source);
                    assert_eq!(line.canonical, canonical);
                    assert_eq!(line.runner_build, RUNNER_BUILD);
                    assert_eq!(provenance_consistent(&written), Ok(line));
                }
            }
        }
        // CRLF frontmatter keeps its own line endings on the inserted line.
        let crlf = with_provenance(
            &command_canonical("x"),
            "---\r\na: 1\r\n---\r\n",
            "builtin",
            None,
        );
        assert!(crlf
            .split_once("\r\n")
            .unwrap()
            .1
            .starts_with(PROVENANCE_KEY));
        assert!(!crlf.contains("\n---\n"), "no LF-only fence introduced");
        // A file with no provenance line is not mistaken for one.
        assert_eq!(strip_provenance("---\ndescription: x\n---\n"), None);
        assert_eq!(strip_provenance("# plain\n"), None);
    }

    /// `canonical_sha` is the ONE optional field: rendered last, parsed when
    /// present, and absent from every non-canonical line.
    #[test]
    fn canonical_sha_is_rendered_and_parsed_only_for_canonical_bodies() {
        let body = "---\nname: x\n---\n# x\n";
        let written = with_provenance(
            &command_canonical("x"),
            body,
            "canonical",
            Some("0123456789ab"),
        );
        let key_line = written.lines().nth(1).unwrap();
        assert!(
            key_line.ends_with(" canonical_sha=0123456789ab"),
            "{key_line}"
        );
        let line = provenance_consistent(&written).expect("consistent");
        assert_eq!(line.source, "canonical");
        assert_eq!(line.canonical_sha.as_deref(), Some("0123456789ab"));
        assert_eq!(strip_provenance(&written).unwrap().1, body);

        let builtin = with_provenance(&command_canonical("x"), body, "builtin", None);
        assert!(!builtin.contains("canonical_sha"));
        assert_eq!(provenance_consistent(&builtin).unwrap().canonical_sha, None);
    }

    #[test]
    fn the_canonical_paths_name_the_claude_config_file() {
        assert_eq!(
            command_canonical("vet-plan"),
            "qontinui-claude-config:.claude/commands/vet-plan.md"
        );
        assert_eq!(
            skill_canonical("coord-revive"),
            "qontinui-claude-config:.claude/skills/coord-revive/SKILL.md"
        );
    }

    #[test]
    fn a_tampered_body_is_a_blob_mismatch() {
        let written = with_provenance(
            &skill_canonical("s"),
            "---\nname: s\n---\n# s\n",
            "builtin",
            None,
        );
        assert!(provenance_consistent(&written).is_ok());
        let tampered = written.replace("# s", "# t");
        assert!(matches!(
            provenance_consistent(&tampered),
            Err(ProvenanceError::BlobMismatch { .. })
        ));
        assert_eq!(
            provenance_consistent("# no provenance\n"),
            Err(ProvenanceError::Missing)
        );
    }
}
