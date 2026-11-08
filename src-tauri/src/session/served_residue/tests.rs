//! The residue rule against real git fixtures. The shapes are the ones
//! qontinui-claude-config `scripts/session-closeable-test.sh` asserts, and the
//! verdicts are the same: the first-occurrence shape and the clone-factory
//! shape close (with residue counted), an edited stamped copy — even one an
//! exclude hides — does not, a session-authored command does not, and a helper
//! whose source no local checkout holds is UNKNOWN.

use super::*;
use crate::provenance::{command_canonical, skill_canonical, with_provenance};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com");
    let out = crate::process_helpers::output_with_timeout(cmd, Duration::from_secs(60))
        .expect("git runs within 60 s");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "--quiet", "-b", "main"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

const SKILL_BODY: &str = "---\nname: demo\ndescription: d\n---\n# Demo\n";
const HELPER: &str = "#!/bin/sh\necho demo\n";
const PY_HELPER: &str = "def f():\n    return 1\n";
const COMMAND_BODY: &str = "---\ndescription: c\n---\n# Cmd\nbody\n";
const AGENT: &str = "---\nname: reviewer\n---\nReview.\n";

/// The workspace: a qontinui-claude-config with an `origin` (so
/// `refs/remotes/origin/main` exists), an empty qontinui-runner, and a
/// consumer repo `app` with one linked worktree — the session's tree.
struct Fixture {
    _tmp: tempfile::TempDir,
    refs: ServedRefs,
    sha12: String,
    primary: PathBuf,
    tree: PathBuf,
}

impl Fixture {
    fn new(track_in_app: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let origin = ws.join("ccfg-origin");
        init(&origin);
        write(
            &origin.join(".claude/skills/demo/SKILL.md"),
            SKILL_BODY.as_bytes(),
        );
        write(
            &origin.join(".claude/skills/demo/helper.sh"),
            HELPER.as_bytes(),
        );
        write(
            &origin.join(".claude/skills/demo/lib/mod_a.py"),
            PY_HELPER.as_bytes(),
        );
        write(
            &origin.join(".claude/commands/cmd.md"),
            COMMAND_BODY.as_bytes(),
        );
        write(&origin.join(".claude/agents/reviewer.md"), AGENT.as_bytes());
        git(&origin, &["add", "-A"]);
        git(&origin, &["commit", "--quiet", "-m", "ccfg"]);
        let ccfg = ws.join("qontinui-claude-config");
        git(
            ws,
            &[
                "clone",
                "--quiet",
                origin.to_str().unwrap(),
                ccfg.to_str().unwrap(),
            ],
        );
        let sha12: String = git(&ccfg, &["rev-parse", "HEAD"])
            .chars()
            .take(12)
            .collect();

        let runner = ws.join("qontinui-runner");
        init(&runner);
        write(&runner.join("README.md"), b"runner\n");
        git(&runner, &["add", "-A"]);
        git(&runner, &["commit", "--quiet", "-m", "runner"]);

        let primary = ws.join("app");
        init(&primary);
        write(&primary.join("README.md"), b"app\n");
        for (rel, body) in track_in_app {
            write(&primary.join(rel), body.as_bytes());
        }
        git(&primary, &["add", "-A"]);
        git(&primary, &["commit", "--quiet", "-m", "app"]);
        let tree = ws.join("wt").join("app");
        git(
            &primary,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                tree.to_str().unwrap(),
            ],
        );

        Self {
            refs: ServedRefs::under(Some(ws)),
            sha12,
            primary,
            tree,
            _tmp: tmp,
        }
    }

    /// What the runner provisions: a stamped command and SKILL.md (canonical
    /// rung at the ccfg sha), the skill's unstamped helpers, and the agent def.
    fn provision(&self) {
        let t = &self.tree;
        write(
            &t.join(".claude/commands/cmd.md"),
            with_provenance(
                &command_canonical("cmd"),
                COMMAND_BODY,
                "canonical",
                Some(&self.sha12),
            )
            .as_bytes(),
        );
        write(
            &t.join(".claude/skills/demo/SKILL.md"),
            with_provenance(
                &skill_canonical("demo"),
                SKILL_BODY,
                "canonical",
                Some(&self.sha12),
            )
            .as_bytes(),
        );
        write(&t.join(".claude/skills/demo/helper.sh"), HELPER.as_bytes());
        write(
            &t.join(".claude/skills/demo/lib/mod_a.py"),
            PY_HELPER.as_bytes(),
        );
        write(&t.join(".claude/agents/reviewer.md"), AGENT.as_bytes());
    }

    /// Run the bundled Python helper the way a session does, leaving its
    /// `__pycache__/` beside it. `false` when no python3 is on PATH.
    fn compile_helper(&self) -> bool {
        let src = self.tree.join(".claude/skills/demo/lib/mod_a.py");
        let mut cmd = Command::new("python3");
        cmd.arg("-c")
            .arg("import py_compile,sys; py_compile.compile(sys.argv[1], doraise=True)")
            .arg(&src);
        crate::process_helpers::output_with_timeout(cmd, Duration::from_secs(60))
            .is_ok_and(|o| o.status.success())
    }

    fn judge(&self) -> ChangesJudgement {
        let status = git(
            &self.tree,
            &["status", "--porcelain", "--untracked-files=normal"],
        );
        judge_changes(
            &self.tree,
            &status,
            None,
            &self.refs,
            &Git::new(Duration::from_secs(30)),
        )
    }
}

/// The first-occurrence shape: nothing tracked under `.claude/`, so `git
/// status` collapses the whole corpus into `?? .claude/` — stamped and
/// unstamped files, an agent def, and a session-run helper's bytecode.
#[test]
fn first_occurrence_shape_closes_with_residue() {
    let fx = Fixture::new(&[]);
    fx.provision();
    let compiled = fx.compile_helper();
    let status = git(
        &fx.tree,
        &["status", "--porcelain", "--untracked-files=normal"],
    );
    assert_eq!(
        status, "?? .claude/",
        "the collapsed shape this test exists for"
    );
    let j = fx.judge();
    assert!(j.is_clean(), "{j:?}");
    assert_eq!(j.residue, 5 + usize::from(compiled), "{j:?}");
}

/// The clone-factory shape: the repo tracks `.claude/agents/<x>.md`, so the
/// porcelain names `?? .claude/commands/` and `?? .claude/skills/` separately.
#[test]
fn clone_factory_shape_closes_with_residue() {
    let fx = Fixture::new(&[(".claude/agents/clone-implementer.md", "tracked\n")]);
    fx.provision();
    let status = git(
        &fx.tree,
        &["status", "--porcelain", "--untracked-files=normal"],
    );
    assert!(
        status.contains("?? .claude/commands/") && status.contains("?? .claude/skills/"),
        "{status}"
    );
    let j = fx.judge();
    assert!(j.is_clean(), "{j:?}");
    assert_eq!(j.residue, 5, "{j:?}");
}

/// An edited stamped copy keeps its stamp; presence is not the property.
#[test]
fn edited_stamped_copy_is_uncommitted() {
    let fx = Fixture::new(&[]);
    fx.provision();
    let path = fx.tree.join(".claude/commands/cmd.md");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("a session's edit\n");
    std::fs::write(&path, &text).unwrap();
    // The mutation partner, stated as a unit: a presence-only check would pass
    // this file, and the stamp verdict must not.
    assert!(has_provenance_key(text.as_bytes()));
    assert!(stamp_verdict(text.as_bytes()).is_err());
    let j = fx.judge();
    let why = j.uncommitted.expect("an edit is work");
    assert!(why.contains(".claude/commands/cmd.md"), "{why}");
}

/// An exclude hides the edited copy from `git status`; the rule still sees it.
#[test]
fn edited_copy_hidden_by_an_exclude_is_uncommitted() {
    let fx = Fixture::new(&[(".claude/agents/clone-implementer.md", "tracked\n")]);
    fx.provision();
    let exclude = fx.primary.join(".git/info/exclude");
    let mut ex = std::fs::read_to_string(&exclude).unwrap_or_default();
    ex.push_str("/.claude/skills/demo/helper.sh\n");
    // `info/` exists only when git's init template made it.
    write(&exclude, ex.as_bytes());
    std::fs::write(
        fx.tree.join(".claude/skills/demo/helper.sh"),
        "#!/bin/sh\nrm -rf /tmp/x\n",
    )
    .unwrap();
    let j = fx.judge();
    let why = j
        .uncommitted
        .expect("an edit hidden by an exclude is still work");
    assert!(why.contains("helper.sh") && why.contains("differ"), "{why}");
}

/// A command the session wrote itself carries no stamp.
#[test]
fn session_authored_command_is_uncommitted() {
    let fx = Fixture::new(&[]);
    fx.provision();
    write(&fx.tree.join(".claude/commands/mine.md"), b"# mine\n");
    let why = fx.judge().uncommitted.expect("authored work");
    assert!(
        why.contains("mine.md") && why.contains("unstamped"),
        "{why}"
    );
}

/// A helper whose SKILL.md names an account source no checkout holds.
#[test]
fn unverifiable_helper_is_unknown() {
    let fx = Fixture::new(&[]);
    fx.provision();
    write(
        &fx.tree.join(".claude/skills/demo/SKILL.md"),
        with_provenance(&skill_canonical("demo"), SKILL_BODY, "served", None).as_bytes(),
    );
    let j = fx.judge();
    assert!(j.uncommitted.is_none(), "{j:?}");
    let why = j.unknown.expect("unverifiable is UNKNOWN");
    assert!(why.contains("source=served"), "{why}");
}

/// A helper whose bytes differ from the commit its skill's stamp names.
#[test]
fn helper_differing_from_its_source_is_uncommitted() {
    let fx = Fixture::new(&[]);
    fx.provision();
    std::fs::write(
        fx.tree.join(".claude/skills/demo/helper.sh"),
        "#!/bin/sh\necho changed\n",
    )
    .unwrap();
    assert!(fx.judge().uncommitted.is_some());
}

/// A symlink under a served folder is never the runner's write.
#[cfg(unix)]
#[test]
fn symlink_is_uncommitted() {
    let fx = Fixture::new(&[]);
    fx.provision();
    std::os::unix::fs::symlink("/etc/hostname", fx.tree.join(".claude/commands/link.md")).unwrap();
    let why = fx.judge().uncommitted.expect("a link is not residue");
    assert!(why.contains("symlink"), "{why}");
}

/// A path outside the served folders is a definite reason, residue or not.
#[test]
fn untracked_file_outside_the_corpus_is_uncommitted() {
    let fx = Fixture::new(&[]);
    fx.provision();
    write(&fx.tree.join("notes.txt"), b"work\n");
    assert!(fx.judge().uncommitted.is_some());
    // And inside a collapsed `.claude/`, a non-served file is found too.
    let fx = Fixture::new(&[]);
    fx.provision();
    write(&fx.tree.join(".claude/settings.local.json"), b"{}\n");
    assert!(fx.judge().uncommitted.is_some());
}

/// An agent def matching no reference, at a path no provisioner ever held.
#[test]
fn never_provisioned_agent_is_uncommitted() {
    let fx = Fixture::new(&[]);
    fx.provision();
    write(&fx.tree.join(".claude/agents/brand-new.md"), b"mine\n");
    let why = fx.judge().uncommitted.expect("never provisioned");
    assert!(why.contains(NEVER), "{why}");
}

/// An agent def that matches no reference at a path the source DOES hold.
#[test]
fn drifted_agent_is_unknown() {
    let fx = Fixture::new(&[]);
    fx.provision();
    write(
        &fx.tree.join(".claude/agents/reviewer.md"),
        b"---\nname: reviewer\n---\nOther.\n",
    );
    // The checkout's working copy holds the path, so the overlay may have written it.
    let j = fx.judge();
    assert!(j.uncommitted.is_none(), "{j:?}");
    assert!(j.unknown.is_some(), "{j:?}");
}

/// Any stderr from `git status` leaves what it skipped unjudged (item 12).
#[test]
fn status_warning_is_unknown_and_a_definite_reason_outranks_it() {
    let fx = Fixture::new(&[]);
    fx.provision();
    let status = git(
        &fx.tree,
        &["status", "--porcelain", "--untracked-files=normal"],
    );
    let git_ = Git::new(Duration::from_secs(30));
    let j = judge_changes(
        &fx.tree,
        &status,
        Some("warning: could not open directory 'x/'"),
        &fx.refs,
        &git_,
    );
    assert!(j.uncommitted.is_none());
    assert!(j.unknown.unwrap().contains("could not open"));
    write(&fx.tree.join("notes.txt"), b"work\n");
    let status = git(
        &fx.tree,
        &["status", "--porcelain", "--untracked-files=normal"],
    );
    let j = judge_changes(&fx.tree, &status, Some("warning: x"), &fx.refs, &git_);
    assert!(j.uncommitted.is_some(), "definite outranks unknown");
}

/// No reference checkouts at all: the canonical helpers cannot be checked.
#[test]
fn no_reference_checkout_is_unknown() {
    let fx = Fixture::new(&[]);
    fx.provision();
    let status = git(
        &fx.tree,
        &["status", "--porcelain", "--untracked-files=normal"],
    );
    let j = judge_changes(
        &fx.tree,
        &status,
        None,
        &ServedRefs::default(),
        &Git::new(Duration::from_secs(30)),
    );
    assert!(j.uncommitted.is_none(), "{j:?}");
    assert!(j.unknown.unwrap().contains("workspace root is unresolved"));
}

/// A clean tree stays clean, and judges nothing.
#[test]
fn clean_tree_is_clean() {
    let fx = Fixture::new(&[]);
    let j = fx.judge();
    assert!(j.is_clean() && j.residue == 0, "{j:?}");
}

/// Bytecode beside a residue source that is not that source's compilation.
#[test]
fn foreign_bytecode_beside_residue_is_not_residue() {
    let fx = Fixture::new(&[]);
    fx.provision();
    let mut cmd = Command::new("python3");
    cmd.args(["-c", "import sys; print(sys.implementation.cache_tag)"]);
    let tag = crate::process_helpers::output_with_timeout(cmd, Duration::from_secs(60));
    let Some(tag) = tag.ok().filter(|o| o.status.success()) else {
        return; // no interpreter here: nothing to compile, nothing to judge
    };
    let tag = String::from_utf8_lossy(&tag.stdout).trim().to_string();
    // A header that opens as bytecode, around a body that is not this source's.
    let mut bytes = b"\x00\x0d\r\n".to_vec();
    bytes.extend_from_slice(&[0u8; 12]);
    bytes.extend_from_slice(b"not code");
    write(
        &fx.tree.join(format!(
            ".claude/skills/demo/lib/__pycache__/mod_a.{tag}.pyc"
        )),
        &bytes,
    );
    let j = fx.judge();
    assert!(!j.is_clean(), "{j:?}");
    // And a file named as bytecode that does not even open as bytecode is work.
    write(
        &fx.tree.join(format!(
            ".claude/skills/demo/lib/__pycache__/mod_b.{tag}.pyc"
        )),
        b"text",
    );
    assert!(fx.judge().uncommitted.is_some());
}

/// The stamp grammar is the closed one a runner renders.
#[test]
fn stamp_grammar_is_closed() {
    let good = with_provenance(&command_canonical("x"), "# x\n", "builtin", None);
    assert!(stamp_fields(good.as_bytes()).is_some());
    assert!(stamp_verdict(good.as_bytes()).is_ok());
    let bad_source = good.replace("source=builtin", "source=elsewhere");
    assert!(stamp_fields(bad_source.as_bytes()).is_none());
    // canonical_sha only with source=canonical.
    let stray_sha = with_provenance(
        &command_canonical("x"),
        "# x\n",
        "builtin",
        Some("0123456789ab"),
    );
    assert!(stamp_fields(stray_sha.as_bytes()).is_none());
    let canonical = with_provenance(
        &command_canonical("x"),
        "# x\n",
        "canonical",
        Some("0123456789ab"),
    );
    assert!(stamp_fields(canonical.as_bytes()).is_some());
}
