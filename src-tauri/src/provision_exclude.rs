//! Keep what the fleet provisioners WROTE out of `git status`.
//!
//! [`crate::fleet_commands`] and [`crate::fleet_skills`] write
//! `.claude/commands/<name>.md` and `.claude/skills/<name>/…` into a spawned
//! session's cwd. When that cwd is a git checkout, every path they create reads
//! as untracked, so the checkout looks dirty: a "fast-forward only if the tree
//! is clean" step skips it, and the runner's own pull executor parks it as
//! `wip_on_default`. Measured 2026-11-14 on a primary `qontinui-studio`: 12
//! untracked entries, all provisioned, and a scheduled fast-forward that had
//! skipped for weeks.
//!
//! The fix is NOT a blanket `.claude/commands/` / `.claude/skills/` entry in
//! `fleet::MANAGED_REPO_EXCLUDES`: in `qontinui-claude-config` (and any repo
//! with repo-local skills) those directories are AUTHORED, and a blanket
//! exclude would hide a human's new untracked skill from `git status`. Instead
//! each provisioning pass appends exactly the paths it wrote to the repository's
//! `info/exclude`:
//!
//! - one anchored line per written command file (`/<prefix>.claude/commands/<name>.md`);
//! - one anchored line per written skill DIRECTORY (`/<prefix>.claude/skills/<name>/`),
//!   unless git tracks anything under that directory — then each written,
//!   untracked file of it is excluded individually instead, so a tracked skill
//!   directory never hides a human's new file beside its tracked ones;
//! - never a path git tracks (exclusion would not affect it, but the line would
//!   misstate what the runner owns);
//! - nothing at all unless the tracked-file probe ANSWERED
//!   ([`TrackedPaths::is_answered`]): outside a work tree there is no exclude
//!   file, and an unanswered probe inside one cannot say which paths are tracked.
//!
//! **Stale lines are pruned.** Each pass removes, from the lines in its OWN
//! marker block anchored under its provisioned root, every line this pass did
//! not produce AND has positive evidence is stale: the path it names is now
//! tracked (a file git tracks, or a directory with anything tracked beneath
//! it), or the path is absent from disk. That covers a path the repository
//! started tracking (a pull vendors the skill, or a person commits the command)
//! — where a stale DIRECTORY line would otherwise hide a person's new file
//! beside the now-tracked ones, because git does not look inside an excluded
//! directory — and a command or skill the provisioner now HOLDS BACK
//! (`RootVersioned`, once the repository versions the provisioned tree; such a
//! path is absent by construction), whose stale line would hide a file a person
//! later creates at that path. A line for a runner-written file that is still
//! on disk and untracked is KEPT even when this pass did not write it (another
//! account's override-only command, a skill this pass refused, a failed write),
//! so that file never reads dirty again and alternating passes do not
//! flip-flop the file. Accepted cost: linked worktrees share one
//! `info/exclude`, so a path absent in one worktree and present in another can
//! lose its line until the next pass in the other worktree re-adds it. "The
//! block" is every line from this module's marker up to the next comment line,
//! so a person's own lines are never touched provided they sit outside it —
//! before the marker, or under a comment header of their own. (An uncommented
//! line a person appends at the end of the file joins the block, and is pruned
//! only on the same evidence.)
//!
//! The exclude file is resolved with `git rev-parse --git-path info/exclude`,
//! so a linked worktree writes to its COMMON dir's `info/exclude` (the one git
//! actually reads for it), and `--show-prefix` anchors entries correctly when
//! the session cwd is a subdirectory of the checkout. Lines are matched whole
//! and trimmed, so a second pass appends nothing.
//!
//! Best-effort by contract: a spawn never fails because of this module. A
//! failure to resolve or to read/write the exclude file is logged at `warn!` and
//! swallowed — a missed exclude only means the tree reads dirty, which is the
//! state before this module existed.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::provision_guard::TrackedPaths;

/// Header line written once above the provisioned-path entries in a repo's
/// `info/exclude`, so an operator can see who added them. Distinct from
/// `fleet::MANAGED_EXCLUDE_MARKER`, which brackets the fixed machine-local roster.
pub(crate) const PROVISIONED_EXCLUDE_MARKER: &str =
    "# qontinui-runner: provisioned .claude commands/skills (auto-excluded, per path)";

/// Compute the exclude entries for one provisioning pass, relative to the
/// provisioned root (`.claude/commands` or `.claude/skills`). A trailing `/`
/// marks a directory entry.
///
/// `written` are the paths the pass wrote, relative to that root (as the
/// provisioners compute them). A single-component path is a file entry; a
/// deeper path contributes its top-level directory, unless git tracks anything
/// under that directory, in which case the file itself is the entry.
fn entries_for(written: &[PathBuf], tracked: &TrackedPaths) -> Vec<String> {
    if !tracked.is_answered() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    for rel in written {
        if tracked.contains(rel) {
            continue;
        }
        let mut comps = rel.components();
        let Some(first) = comps.next() else { continue };
        let Some(first) = first.as_os_str().to_str() else {
            continue;
        };
        let file_entry = || rel.to_str().map(|s| s.replace('\\', "/"));
        if comps.next().is_none() {
            if let Some(s) = file_entry() {
                push(s);
            }
        } else if !tracked.tracks_any_under(Path::new(first)) {
            push(format!("{first}/"));
        } else if let Some(s) = file_entry() {
            push(s);
        }
    }
    out
}

/// Escape a repo-relative path for use as a literal, anchored gitignore
/// pattern: the glob metacharacters and the escape character itself. A path
/// holding a newline or carriage return cannot be written as one line at all
/// and yields `None`.
fn escape_pattern(path: &str) -> Option<String> {
    if path.contains('\n') || path.contains('\r') {
        return None;
    }
    let mut out = String::with_capacity(path.len() + 1);
    out.push('/');
    for c in path.chars() {
        if matches!(c, '\\' | '*' | '?' | '[') {
            out.push('\\');
        }
        out.push(c);
    }
    // A trailing space is stripped by git unless escaped.
    if out.ends_with(' ') {
        out.pop();
        out.push_str("\\ ");
    }
    Some(out)
}

/// Inverse of [`escape_pattern`] for the part after the anchor: drop each
/// escaping backslash.
fn unescape(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// True iff `line` (trimmed) is an anchored pattern under `root_pattern` (the
/// escaped, anchored provisioned root, ending in `/`) that this pass did NOT
/// produce and that is provably stale: the path it names, relative to `base`,
/// is tracked (a tracked file, or a directory with anything tracked beneath
/// it) or absent from disk — which is also every path held back as
/// `RootVersioned`. Only ever asked about lines inside this module's own
/// marker block (see [`update_exclude`]).
fn is_stale(
    line: &str,
    root_pattern: &str,
    current: &std::collections::HashSet<&str>,
    base: &Path,
    tracked: &TrackedPaths,
) -> bool {
    let Some(rest) = line.strip_prefix(root_pattern) else {
        return false;
    };
    if rest.is_empty() || current.contains(line) {
        return false;
    }
    // The runner only writes literal paths; a line with an unescaped glob
    // metacharacter is a person's pattern, never pruned.
    let mut escaped = false;
    for c in rest.chars() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if matches!(c, '*' | '?' | '[') {
            return false;
        }
    }
    let rel = unescape(rest);
    let (rel_path, is_tracked) = match rel.strip_suffix('/') {
        Some(dir) => (Path::new(dir), tracked.tracks_any_under(Path::new(dir))),
        None => (Path::new(rel.as_str()), tracked.contains(Path::new(&rel))),
    };
    is_tracked || !base.join(rel_path).exists()
}

/// Where `base`'s repository keeps its `info/exclude`, and `base`'s path
/// relative to the work tree root (`""` at the root, else ending in `/`).
/// `None` (logged at `warn!`) when git could not answer — callers only ask
/// from inside a work tree, so any failure here is worth seeing.
fn resolve_exclude(base: &Path) -> Option<(PathBuf, String)> {
    let stdout = match crate::provision_guard::run_bounded_git(
        base,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/exclude",
            "--show-prefix",
        ],
    ) {
        Ok(stdout) => stdout,
        Err(cause) => {
            warn!(
                base = %base.display(),
                %cause,
                "provision exclude: could not resolve the repository's info/exclude; \
                 provisioned paths are not excluded this pass"
            );
            return None;
        }
    };
    let Ok(text) = String::from_utf8(stdout) else {
        warn!(
            base = %base.display(),
            "provision exclude: git rev-parse printed non-UTF-8 output; nothing excluded"
        );
        return None;
    };
    let mut lines = text.split('\n');
    let exclude = PathBuf::from(lines.next().map(str::trim_end).unwrap_or_default());
    let prefix = lines.next().map(str::trim_end).unwrap_or_default();
    // A git older than 2.31 does not know `--path-format` and ECHOES it as the
    // first line while still exiting 0. Accept only what the flag promises —
    // an absolute path ending in `info/exclude` — so such a git can never make
    // this write a relative file into the runner's own working directory.
    if !exclude.is_absolute() || !exclude.ends_with("info/exclude") {
        warn!(
            base = %base.display(),
            answer = %exclude.display(),
            "provision exclude: git rev-parse did not return an absolute info/exclude \
             path (git older than 2.31?); nothing excluded"
        );
        return None;
    }
    Some((exclude, prefix.to_string()))
}

/// The trimmed, non-comment lines INSIDE this module's blocks: from each
/// [`PROVISIONED_EXCLUDE_MARKER`] to the next comment line. Only these are
/// candidates for pruning, so a person's own line naming the same path, and
/// another writer's block, are never removed.
fn block_lines(existing: &str) -> impl Iterator<Item = &str> {
    let mut in_block = false;
    existing.lines().filter_map(move |line| {
        let t = line.trim();
        if t.starts_with('#') {
            in_block = t == PROVISIONED_EXCLUDE_MARKER;
            None
        } else if in_block {
            Some(t)
        } else {
            None
        }
    })
}

/// Bring `exclude_path` up to date: drop every line `stale` flags, and append
/// every pattern of `patterns` not already present as a whole (trimmed) line,
/// under [`PROVISIONED_EXCLUDE_MARKER`]. Returns `(appended, pruned)`.
///
/// Holds [`crate::fleet::info_exclude_lock`] across the whole
/// read-modify-write, like every other in-process writer of an
/// `info/exclude` (the machine-local roster heal and the shared-branch
/// allocation block), so neither this pass nor theirs loses the other's edit.
/// The file is read through [`crate::fleet::read_info_exclude`]: one that
/// exists but cannot be read, or is not UTF-8, is an `Err` and is never
/// rewritten.
///
/// `stale` stats paths under the session cwd, which can sit on a slow mount,
/// so the whole block is judged from an unlocked read first and the locked
/// pass considers only the lines judged stale there (usually none). Each of
/// those is judged AGAIN under the lock before it is pruned, so a path a
/// concurrent pass re-created between the two reads keeps its line. A line
/// that appeared between the two reads was not judged and is kept; the next
/// pass judges it.
fn update_exclude(
    exclude_path: &Path,
    patterns: &[String],
    stale: impl Fn(&str) -> bool,
) -> std::io::Result<(usize, usize)> {
    let judged_stale: std::collections::HashSet<String> = {
        let unlocked =
            crate::fleet::read_info_exclude(exclude_path).map_err(std::io::Error::other)?;
        block_lines(&unlocked)
            .filter(|t| stale(t))
            .map(str::to_string)
            .collect()
    };
    let _excl = crate::fleet::info_exclude_lock();
    let existing = crate::fleet::read_info_exclude(exclude_path).map_err(std::io::Error::other)?;
    let mut in_block = false;
    let mut kept: Vec<&str> = Vec::new();
    let mut pruned = 0usize;
    for line in existing.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            in_block = t == PROVISIONED_EXCLUDE_MARKER;
        } else if in_block && judged_stale.contains(t) && stale(t) {
            pruned += 1;
            continue;
        }
        kept.push(line);
    }
    let present: std::collections::HashSet<&str> = kept.iter().map(|l| l.trim()).collect();
    let missing: Vec<&String> = patterns
        .iter()
        .filter(|p| !present.contains(p.trim()))
        .collect();
    if missing.is_empty() && pruned == 0 {
        return Ok((0, 0));
    }
    let mut chunk = String::new();
    // Appended lines land at the END of the file, so they belong to this
    // module's block only if the last comment header there is its marker;
    // otherwise (no block yet, or another writer's block follows it) open a
    // new one, so later passes can still prune what they appended.
    let last_header = kept
        .iter()
        .rev()
        .map(|l| l.trim())
        .find(|t| t.starts_with('#'));
    if last_header != Some(PROVISIONED_EXCLUDE_MARKER) && !missing.is_empty() {
        chunk.push_str(PROVISIONED_EXCLUDE_MARKER);
        chunk.push('\n');
    }
    for p in &missing {
        chunk.push_str(p);
        chunk.push('\n');
    }
    if let Some(parent) = exclude_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if pruned > 0 {
        // Rare: a whole-file rewrite, only when something went stale. The
        // exclude lock keeps every in-process writer out between the read and
        // the write; a writer in ANOTHER runner process can still append in
        // that window and lose it. The roster heal and this module re-add
        // their lines on their next pass. A shared-branch allocation block is
        // added once and is NOT re-added: its restore then reads the checkout
        // as Dirty and keeps it, which is the safe side.
        // A marker left with no entry under it (every line of its block was
        // pruned) is dropped too, so repeated prunes do not pile up headers.
        let mut lines: Vec<&str> = Vec::with_capacity(kept.len());
        for (i, line) in kept.iter().enumerate() {
            let is_empty_marker = line.trim() == PROVISIONED_EXCLUDE_MARKER
                && kept[i + 1..]
                    .iter()
                    .map(|l| l.trim())
                    .find(|t| !t.is_empty())
                    .is_none_or(|t| t.starts_with('#'));
            if !is_empty_marker {
                lines.push(line);
            }
        }
        let mut out = lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        // Re-derive the marker decision against what is actually written.
        let last_header = lines
            .iter()
            .rev()
            .map(|l| l.trim())
            .find(|t| t.starts_with('#'));
        if !missing.is_empty()
            && last_header != Some(PROVISIONED_EXCLUDE_MARKER)
            && !chunk.starts_with(PROVISIONED_EXCLUDE_MARKER)
        {
            out.push_str(PROVISIONED_EXCLUDE_MARKER);
            out.push('\n');
        }
        out.push_str(&chunk);
        // Atomic replace: a concurrent `git status` never reads a truncated
        // file, and a crash mid-write cannot lose a person's lines.
        let tmp =
            exclude_path.with_file_name(format!("exclude.qontinui-tmp.{}", std::process::id()));
        std::fs::write(&tmp, out)?;
        if let Err(e) = std::fs::rename(&tmp, exclude_path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    } else {
        // The common path appends, so it never drops another writer's lines.
        // Under the exclude lock no in-process whole-file rewrite (the roster
        // heal, the shared-branch block's add / remove / sweep, or a pruning
        // pass) can interleave with it; one in another runner process can
        // still drop it, and the next spawn re-adds it. Two concurrent appends
        // across processes can at worst repeat a line, which git ignores.
        if !existing.is_empty() && !existing.ends_with('\n') {
            chunk.insert(0, '\n');
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(exclude_path)?;
        f.write_all(chunk.as_bytes())?;
    }
    Ok((missing.len(), pruned))
}

/// Exclude what one provisioning pass wrote under `base` (the provisioned root,
/// e.g. `<cwd>/.claude/skills`) from `git status` of the enclosing repository,
/// and prune lines under that root that this pass did not produce and that
/// name a now-tracked or absent path.
///
/// `written` are paths relative to `base`; `tracked` is the probe the pass
/// already ran over `base` BEFORE writing. Best-effort: never panics, never
/// propagates — see the module doc.
pub(crate) fn exclude_written_paths(
    provisioner: &str,
    base: &Path,
    written: &[PathBuf],
    tracked: &TrackedPaths,
) {
    // Not answered: outside a work tree (nothing to exclude in), or an
    // unanswered probe inside one (nothing says what is tracked). The guard
    // already logged the latter.
    if !tracked.is_answered() {
        return;
    }
    let Some((exclude_path, prefix)) = resolve_exclude(base) else {
        return;
    };
    // `base` is always a provisioned root below the work tree root, so the
    // prefix is never empty; refuse to prune against the bare `/` anchor if it
    // ever is.
    if prefix.is_empty() {
        return;
    }
    let Some(root_pattern) = escape_pattern(&prefix) else {
        return;
    };
    let patterns: Vec<String> = entries_for(written, tracked)
        .iter()
        .filter_map(|e| escape_pattern(&format!("{prefix}{e}")))
        .collect();
    // Trimmed, because `update_exclude` asks about trimmed lines; a pattern
    // ending in an escaped space would otherwise never match itself.
    let current: std::collections::HashSet<&str> = patterns.iter().map(|p| p.trim()).collect();
    match update_exclude(&exclude_path, &patterns, |line| {
        is_stale(line, &root_pattern, &current, base, tracked)
    }) {
        Ok((0, 0)) => {}
        Ok((added, pruned)) => info!(
            "{provisioner}: excluded {added} provisioned path(s) from git status, pruned \
             {pruned} line(s) this pass no longer writes, in {}",
            exclude_path.display()
        ),
        Err(e) => warn!(
            "{provisioner}: could not record provisioned paths in {} ({e}) — \
             continuing; the checkout may read dirty with untracked .claude paths",
            exclude_path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provision_guard::test_support::{assert_not_in_any_repo, git_add, git_init};

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = crate::process_helpers::no_window("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn untracked(dir: &Path) -> String {
        git(dir, &["status", "--porcelain", "--untracked-files=all"])
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn exclude_lines(repo: &Path) -> Vec<String> {
        std::fs::read_to_string(repo.join(".git").join("info").join("exclude"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Provision-shaped fixture: write `files` (relative to `base`), probe as
    /// the provisioners do BEFORE writing, then exclude what was written.
    fn provision(base: &Path, files: &[&str]) {
        let tracked = TrackedPaths::probe(base);
        let mut written = Vec::new();
        for f in files {
            write(&base.join(f), "x\n");
            written.push(PathBuf::from(f));
        }
        exclude_written_paths("test", base, &written, &tracked);
    }

    #[test]
    fn a_written_path_is_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        let commands = tmp.path().join(".claude").join("commands");
        let skills = tmp.path().join(".claude").join("skills");
        std::fs::create_dir_all(&commands).unwrap();
        std::fs::create_dir_all(&skills).unwrap();

        provision(&commands, &["gate.md"]);
        provision(&skills, &["preflight/SKILL.md", "preflight/run.sh"]);

        let lines = exclude_lines(tmp.path());
        assert!(
            lines.contains(&"/.claude/commands/gate.md".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"/.claude/skills/preflight/".to_string()),
            "{lines:?}"
        );
        assert!(lines.contains(&PROVISIONED_EXCLUDE_MARKER.to_string()));
        assert_eq!(
            untracked(tmp.path()),
            "",
            "the provisioned paths no longer read dirty"
        );
    }

    #[test]
    fn a_second_provision_adds_no_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        let commands = tmp.path().join(".claude").join("commands");
        std::fs::create_dir_all(&commands).unwrap();

        provision(&commands, &["gate.md", "policy.md"]);
        let first = exclude_lines(tmp.path());
        provision(&commands, &["gate.md", "policy.md"]);
        assert_eq!(
            exclude_lines(tmp.path()),
            first,
            "a second pass appends nothing"
        );
        let marker_count = first
            .iter()
            .filter(|l| *l == PROVISIONED_EXCLUDE_MARKER)
            .count();
        assert_eq!(marker_count, 1);
    }

    #[test]
    fn a_tracked_path_is_never_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        let commands = tmp.path().join(".claude").join("commands");
        let skills = tmp.path().join(".claude").join("skills");
        // A tracked command, and a skill directory holding one tracked file.
        let tracked_cmd = commands.join("gate.md");
        write(&tracked_cmd, "repo's own\n");
        git_add(tmp.path(), &tracked_cmd);
        let tracked_skill_file = skills.join("launch-kit").join("SKILL.md");
        write(&tracked_skill_file, "repo's own\n");
        git_add(tmp.path(), &tracked_skill_file);

        // Even if a pass reports them written (a tracked-but-deleted path is
        // rewritten by the provisioners), no line may name them.
        provision(&commands, &["gate.md", "policy.md"]);
        provision(&skills, &["launch-kit/SKILL.md", "launch-kit/helper.sh"]);

        let lines = exclude_lines(tmp.path());
        assert!(
            !lines.contains(&"/.claude/commands/gate.md".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"/.claude/commands/policy.md".to_string()),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l == "/.claude/skills/launch-kit/"),
            "a directory holding a tracked file is never excluded whole: {lines:?}"
        );
        assert!(!lines.contains(&"/.claude/skills/launch-kit/SKILL.md".to_string()));
        assert!(lines.contains(&"/.claude/skills/launch-kit/helper.sh".to_string()));

        // A human's new file beside the tracked skill still shows.
        write(&skills.join("launch-kit").join("notes.md"), "mine\n");
        assert!(untracked(tmp.path()).contains("notes.md"));
    }

    #[test]
    fn a_line_that_went_stale_is_pruned_and_a_persons_own_line_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        let skills = tmp.path().join(".claude").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        provision(&skills, &["preflight/SKILL.md"]);
        assert!(exclude_lines(tmp.path()).contains(&"/.claude/skills/preflight/".to_string()));

        // Another writer's block follows ours, then a person's own line.
        let exclude = tmp.path().join(".git").join("info").join("exclude");
        let mut text = std::fs::read_to_string(&exclude).unwrap();
        text.push_str("# someone else\n/.claude/skills/preflight/\n");
        std::fs::write(&exclude, text).unwrap();

        // The repo starts tracking the skill (a pull vendored it).
        // `-f`: the path is currently excluded, which is the point.
        git(
            tmp.path(),
            &["add", "-f", ".claude/skills/preflight/SKILL.md"],
        );
        provision(&skills, &["preflight/SKILL.md", "preflight/run.sh"]);

        let lines = exclude_lines(tmp.path());
        let dir_lines = lines
            .iter()
            .filter(|l| *l == "/.claude/skills/preflight/")
            .count();
        assert_eq!(
            dir_lines, 1,
            "ours is pruned, the person's is kept: {lines:?}"
        );
        let pos = |needle: &str| lines.iter().position(|l| l == needle).unwrap();
        assert!(
            pos("/.claude/skills/preflight/") > pos("# someone else"),
            "the surviving directory line is the person's: {lines:?}"
        );
        // The untracked helper is now excluded per file, inside a block of ours.
        let helper = pos("/.claude/skills/preflight/run.sh");
        assert_eq!(lines[helper - 1], PROVISIONED_EXCLUDE_MARKER, "{lines:?}");
        // The emptied first block's marker is dropped, not left as an orphan.
        let markers = lines
            .iter()
            .filter(|l| *l == PROVISIONED_EXCLUDE_MARKER)
            .count();
        assert_eq!(markers, 1, "{lines:?}");
        assert!(
            !exclude
                .with_file_name(format!("exclude.qontinui-tmp.{}", std::process::id()))
                .exists(),
            "the atomic-replace temp file is renamed away"
        );
    }

    /// A commands pass prunes only under its own root, and only on positive
    /// evidence: a line for a command it did not write this pass is dropped
    /// when that file is ABSENT, kept when the file is still on disk untracked
    /// (another account's override-only command), and the skills lines in the
    /// same marker block are never touched.
    #[test]
    fn a_pass_prunes_only_absent_or_tracked_paths_under_its_own_root() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        let commands = tmp.path().join(".claude").join("commands");
        let skills = tmp.path().join(".claude").join("skills");
        std::fs::create_dir_all(&commands).unwrap();
        std::fs::create_dir_all(&skills).unwrap();
        provision(&commands, &["gate.md", "acct-only.md", "gone.md"]);
        provision(&skills, &["preflight/SKILL.md"]);

        // Next pass writes only gate.md: acct-only.md stays on disk (another
        // account's command), gone.md is absent (held back).
        std::fs::remove_file(commands.join("gone.md")).unwrap();
        // A person's glob appended at the end of the file lands inside the
        // runner's block; it names no literal path and is never pruned.
        let exclude = tmp.path().join(".git").join("info").join("exclude");
        let mut text = std::fs::read_to_string(&exclude).unwrap();
        text.push_str("/.claude/commands/*.tmp\n");
        std::fs::write(&exclude, text).unwrap();
        provision(&commands, &["gate.md"]);

        let lines = exclude_lines(tmp.path());
        assert!(
            lines.contains(&"/.claude/commands/gate.md".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"/.claude/commands/acct-only.md".to_string()),
            "an untracked file still on disk keeps its line: {lines:?}"
        );
        assert!(
            !lines.contains(&"/.claude/commands/gone.md".to_string()),
            "an absent path loses its line: {lines:?}"
        );
        assert!(
            lines.contains(&"/.claude/skills/preflight/".to_string()),
            "a commands pass never touches skills lines: {lines:?}"
        );
        assert!(
            lines.contains(&"/.claude/commands/*.tmp".to_string()),
            "a person's glob line is kept: {lines:?}"
        );
        assert_eq!(untracked(tmp.path()), "");
    }

    #[test]
    fn a_human_authored_sibling_skill_still_shows_in_status() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        let skills = tmp.path().join(".claude").join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        provision(&skills, &["preflight/SKILL.md"]);
        write(&skills.join("my-new-skill").join("SKILL.md"), "mine\n");
        let status = untracked(tmp.path());
        assert!(status.contains("my-new-skill"), "{status}");
        assert!(!status.contains("preflight"), "{status}");
    }

    #[test]
    fn a_non_git_cwd_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        assert_not_in_any_repo(tmp.path());
        let commands = tmp.path().join(".claude").join("commands");
        std::fs::create_dir_all(&commands).unwrap();
        provision(&commands, &["gate.md"]);
        assert!(
            !tmp.path().join(".git").exists(),
            "nothing git-shaped is created"
        );
        assert!(commands.join("gate.md").exists());
    }

    #[test]
    fn a_subdirectory_cwd_is_anchored_at_its_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        let commands = tmp.path().join("sub").join(".claude").join("commands");
        std::fs::create_dir_all(&commands).unwrap();
        provision(&commands, &["gate.md"]);
        assert!(exclude_lines(tmp.path()).contains(&"/sub/.claude/commands/gate.md".to_string()));
        assert_eq!(untracked(tmp.path()), "");
    }

    #[test]
    fn a_linked_worktree_writes_the_common_exclude() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        git_init(&main);
        write(&main.join("README"), "x\n");
        git(&main, &["add", "README"]);
        git(&main, &["commit", "--quiet", "-m", "init"]);
        let wt = tmp.path().join("wt");
        git(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                wt.to_str().unwrap(),
            ],
        );

        let commands = wt.join(".claude").join("commands");
        std::fs::create_dir_all(&commands).unwrap();
        provision(&commands, &["gate.md"]);

        assert!(exclude_lines(&main).contains(&"/.claude/commands/gate.md".to_string()));
        assert_eq!(untracked(&wt), "", "the linked worktree reads clean");
    }

    #[test]
    fn an_unknown_probe_excludes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert_not_in_any_repo(tmp.path());
        // A broken `.git` file: the probe fails INSIDE a repository.
        std::fs::write(
            tmp.path().join(".git"),
            b"gitdir: /nonexistent/qontinui-provision-exclude-test\n",
        )
        .unwrap();
        let commands = tmp.path().join("commands");
        std::fs::create_dir_all(&commands).unwrap();
        let tracked = TrackedPaths::probe(&commands);
        assert!(tracked.is_unknown_in_repo());
        assert!(entries_for(&[PathBuf::from("a.md")], &tracked).is_empty());

        // The default (never probed) is unanswered too.
        assert!(entries_for(&[PathBuf::from("a.md")], &TrackedPaths::default()).is_empty());

        // An answered, empty probe does produce entries.
        let repo = tempfile::tempdir().unwrap();
        git_init(repo.path());
        let answered = TrackedPaths::probe(repo.path());
        assert!(answered.is_answered());
        assert_eq!(
            entries_for(
                &[
                    PathBuf::from("a.md"),
                    PathBuf::from("s/SKILL.md"),
                    PathBuf::from("s/x.sh")
                ],
                &answered
            ),
            vec!["a.md".to_string(), "s/".to_string()]
        );
    }

    #[test]
    fn patterns_are_anchored_and_escaped() {
        assert_eq!(
            escape_pattern(".claude/commands/a.md").unwrap(),
            "/.claude/commands/a.md"
        );
        assert_eq!(
            escape_pattern("x/a*b?[c]\\").unwrap(),
            "/x/a\\*b\\?\\[c]\\\\"
        );
        assert_eq!(escape_pattern("a ").unwrap(), "/a\\ ");
        assert!(escape_pattern("a\nb").is_none());
    }

    /// The update's read-modify-write runs under
    /// [`crate::fleet::info_exclude_lock`]: a line another writer appends while
    /// holding the lock survives this update's whole-file prune rewrite. A
    /// writer that read the file before taking the lock would rewrite from
    /// that stale read and drop the appended line. (The 300 ms hold lets the
    /// writer thread reach the lock; on a box too loaded to schedule it in
    /// that window a lock-free regression could pass one run, but correct
    /// code never fails.)
    #[test]
    fn a_line_appended_under_the_lock_survives_a_prune_rewrite() {
        let tmp = tempfile::tempdir().unwrap();
        let exclude = tmp.path().join("info").join("exclude");
        let seeded = format!("# a person's own line\n{PROVISIONED_EXCLUDE_MARKER}\n/gone\n");
        write(&exclude, &seeded);

        let held = crate::fleet::info_exclude_lock();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let writer = {
            let exclude = exclude.clone();
            std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                update_exclude(&exclude, &[], |line| line == "/gone")
            })
        };
        started_rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(
            std::fs::read_to_string(&exclude).unwrap(),
            seeded,
            "nothing is written while another writer holds the exclude lock"
        );
        // Another writer's edit, made under the lock.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&exclude)
                .unwrap();
            f.write_all(b"# another writer\n/foreign\n").unwrap();
        }
        drop(held);

        assert_eq!(writer.join().unwrap().unwrap(), (0, 1));
        let lines: Vec<String> = std::fs::read_to_string(&exclude)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(
            lines,
            vec![
                "# a person's own line".to_string(),
                "# another writer".to_string(),
                "/foreign".to_string(),
            ],
            "the stale line is pruned (and its empty marker with it) and the line \
             appended under the lock survives"
        );
    }

    /// A file that cannot be read faithfully is refused, never rewritten:
    /// content that is not UTF-8 is left byte-identical. (It was refused before
    /// `read_info_exclude` too; this guards against a lossy or empty-string
    /// fallback, under which the prune would rewrite the file.)
    #[test]
    fn a_non_utf8_exclude_is_left_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let exclude = tmp.path().join("info").join("exclude");
        std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
        let bytes = b"# mine\n/keep\xff\n".to_vec();
        std::fs::write(&exclude, &bytes).unwrap();
        let res = update_exclude(&exclude, &["/.claude/commands/gate.md".to_string()], |_| {
            true
        });
        assert!(res.is_err(), "{res:?}");
        assert_eq!(std::fs::read(&exclude).unwrap(), bytes);
    }

    /// A line judged stale from the unlocked read is judged again under the
    /// lock: if its path came back in between (a concurrent pass re-wrote the
    /// file and found the line already present), the line is kept.
    #[test]
    fn a_line_whose_path_came_back_before_the_lock_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let exclude = tmp.path().join("info").join("exclude");
        let seeded = format!("{PROVISIONED_EXCLUDE_MARKER}\n/back\n");
        write(&exclude, &seeded);
        // Stale at the unlocked judgement, present again by the locked one.
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let res = update_exclude(&exclude, &[], |line| {
            line == "/back" && calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
        });
        assert_eq!(res.unwrap(), (0, 0));
        assert_eq!(std::fs::read_to_string(&exclude).unwrap(), seeded);
    }

    /// The cross-module contract with the shared-branch allocation block
    /// (`agent_worktree::shared_branch_restore`): this module's prune rewrite
    /// (which also turns CRLF into LF) keeps that block intact, and that
    /// block's byte-exact removal leaves this module's lines alone.
    #[test]
    fn a_prune_keeps_a_shared_branch_block_and_its_removal_keeps_these_lines() {
        let tmp = tempfile::tempdir().unwrap();
        git_init(tmp.path());
        let exclude = tmp.path().join(".git").join("info").join("exclude");
        write(&exclude, "# mine\r\n/keep\r\n");
        let commands = tmp.path().join(".claude").join("commands");
        std::fs::create_dir_all(&commands).unwrap();

        provision(&commands, &["a.md", "b.md"]);
        crate::agent_worktree::shared_branch_restore::add_allocation_excludes_for_test(
            tmp.path(),
            "agent/x",
        );
        let with_block = std::fs::read_to_string(&exclude).unwrap();
        assert!(with_block.contains("agent/x"), "{with_block:?}");

        // b.md goes away, so the next pass prunes its line: a whole-file rewrite.
        std::fs::remove_file(commands.join("b.md")).unwrap();
        let tracked = TrackedPaths::probe(&commands);
        exclude_written_paths("test", &commands, &[PathBuf::from("a.md")], &tracked);
        let pruned = std::fs::read_to_string(&exclude).unwrap();
        assert!(!pruned.contains("/.claude/commands/b.md"), "{pruned:?}");
        assert!(pruned.contains("/.claude/commands/a.md"), "{pruned:?}");
        // The shared-branch block came through the rewrite whole: both its
        // bracketing lines and every entry, now LF-terminated.
        let pruned_lines: Vec<&str> = pruned.lines().collect();
        assert_eq!(
            pruned_lines
                .iter()
                .filter(|l| l.contains("agent/x"))
                .count(),
            2,
            "begin and end lines survive the prune: {pruned:?}"
        );
        for entry in [".claude/commands/", ".claude/skills/", ".claude/agents/"] {
            assert!(
                pruned_lines.contains(&entry),
                "{entry} survives: {pruned:?}"
            );
        }
        assert!(
            !pruned.contains('\r'),
            "the prune rewrite is LF: {pruned:?}"
        );

        crate::agent_worktree::shared_branch_restore::remove_allocation_excludes(
            tmp.path(),
            "agent/x",
        );
        let lines = exclude_lines(tmp.path());
        assert!(
            !lines.iter().any(|l| l.contains("agent/x")),
            "the block is removed whole: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l == ".claude/commands/"),
            "the block's entries leave with it: {lines:?}"
        );
        assert_eq!(
            lines,
            vec![
                "# mine".to_string(),
                "/keep".to_string(),
                PROVISIONED_EXCLUDE_MARKER.to_string(),
                "/.claude/commands/a.md".to_string(),
            ]
        );
    }
}
