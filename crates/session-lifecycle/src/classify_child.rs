//! `classify_child`: is this direct child of a `claude` doing work, is it the
//! session's own MCP shim, or can it not be told?
//!
//! Served `finished-criteria` condition 2 (v11, amended 2026-11-04, finding
//! `023a0bdd`) reads "no background child" as no child doing WORK: a child this
//! classifier positively identifies as the session's own MCP shim or message pump
//! is not background work, and a child it cannot classify is UNKNOWN and keeps
//! the session open. The clause names what "serves" means, and every rule below
//! is one of its terms:
//!
//! - **A measured-subtree match.** A child is [`ChildClass::ShimOrPump`] only when
//!   its WHOLE subtree — every descendant, with its image and command line —
//!   matches one chain that was measured on a real host and is recorded as a
//!   fixture under `fixtures/classify_child/`. There is no pattern that admits a
//!   chain nobody measured: a shim launched some other way reads as work until
//!   its chain is measured and added here with its fixture.
//! - **pid + creation-time binding.** Every member of the subtree is bound by its
//!   pid AND its creation time from the first table, and re-confirmed against a
//!   FRESH table read after the command lines were: same pid, same creation time,
//!   same parent, and the same membership. A pid reused between the reads, a
//!   member that left, or one that arrived makes the child [`ChildClass::Unknown`].
//! - **Unreadable is UNKNOWN.** A stale table, an unknown creation time, an
//!   unreadable image or command line (or one not in its platform's [`Argv`]
//!   form), or a command-line read that failed outright — none of them can admit
//!   a child.
//!
//! [`ChildClass::Work`] is a positive reading: everything needed was read and the
//! child matches no measured chain — or the child's own image is a tool shell or
//! a nested `claude`, which is always work and is settled from that image alone,
//! so a busy shell's large or changing subtree cannot turn it into UNKNOWN.
//! [`ChildClass::Unknown`] is the absence of a reading. Both keep a session open;
//! they are kept apart so a log can say which.
//!
//! ## The measured chains
//!
//! - **Linux** ([`CHAIN_LINUX_SHIM`]), merytshost 2026-11-08: `claude` → one
//!   `python3` whose argv is `/usr/bin/python3`, `…/scripts/coord-mcp-shim.py`,
//!   `--credential`, `<path>`, with no descendants. The argv is compared element
//!   by element as `/proc/<pid>/cmdline` separates it ([`Argv::Elements`]), never
//!   as a rejoined string.
//! - **Windows** ([`CHAIN_WINDOWS_CMD_SHIM`]), spaceship 2026-11-08: Claude Code
//!   wraps a stdio MCP server in `cmd.exe /d /s /c "<command>"`, and the
//!   WindowsApps `python3.exe` alias adds a hop to the real interpreter, so the
//!   subtree is `cmd.exe` → { `conhost.exe 0x4`, `python3.exe <shim>` →
//!   `python.exe <shim>` }. Each member's command line is its PEB's raw text
//!   ([`Argv::Line`]), split here by the `CommandLineToArgvW` rules.
//!
//! How often each chain occurred on the host it was measured on is recorded,
//! dated, in that fixture's `census` field — not here, where a count could not
//! be re-measured.
//!
//! No message-pump chain has been measured, so none is admitted; the class keeps
//! the clause's name so the pump joins it when one is.

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The Linux chain's id, as it appears in a verdict and a log line.
pub const CHAIN_LINUX_SHIM: &str = "linux-coord-mcp-shim";
/// The Windows chain's id.
pub const CHAIN_WINDOWS_CMD_SHIM: &str = "windows-cmd-coord-mcp-shim";

/// The path suffix every measured shim chain runs, compared with `/` separators
/// (and case-insensitively, on Windows only). It names the measured location —
/// a `qontinui-claude-config` checkout's `scripts/` — so a copy of the script
/// anywhere else (`/tmp/scripts/coord-mcp-shim.py`, a scratch dir's) is not the
/// measured chain. It does NOT pin which checkout: a coord agent worktree's
/// `agent-worktrees/<id>/qontinui-claude-config/scripts/coord-mcp-shim.py` ends
/// with this suffix and is admitted exactly like the primary checkout's.
const SHIM_SCRIPT_SUFFIX: &str = "/qontinui-claude-config/scripts/coord-mcp-shim.py";

/// A subtree larger than this is not walked: it matches no measured chain (the
/// largest has four members), and a pathological table must not cost a census.
const MAX_SUBTREE: usize = 64;

/// Images that are always work: the shells Claude Code's Bash tool runs commands
/// under, and a nested agent. Normalized (lowercase, no path, no `.exe`).
const WORK_IMAGES: [&str; 14] = [
    "bash",
    "sh",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "tcsh",
    "csh",
    "pwsh",
    "powershell",
    "cmd",
    "nu",
    "busybox",
    "claude",
];

/// Which host's measured chains apply. A chain is admitted only on the platform
/// it was measured on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Windows,
    /// No chain has been measured here: nothing is admitted.
    Other,
}

impl Platform {
    /// The platform this crate was compiled for.
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::Other
        }
    }
}

/// One process, as a table read saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcRow {
    pub ppid: u32,
    /// The image name (`cmd.exe`, `python3`), when the read resolved it.
    pub image: Option<String>,
    /// An opaque creation-time stamp, compared only for equality between the
    /// two table reads, which must therefore stamp in the same unit: the
    /// kernel's FILETIME (100 ns) on Windows, `/proc/<pid>/stat` field 22 (the
    /// start time in clock ticks since boot) on Linux. `None` (or `0`) is
    /// UNKNOWN and binds nothing.
    pub start: Option<u64>,
}

/// One process-table read: rows by pid, and whether the read was fresh.
#[derive(Debug, Clone, Default)]
pub struct ProcTable {
    /// `false` when this is not a fresh read (a memo served after a failure, an
    /// empty degrade): nothing read from it can admit a child.
    pub fresh: bool,
    rows: HashMap<u32, ProcRow>,
    children: HashMap<u32, Vec<u32>>,
}

impl ProcTable {
    pub fn new(fresh: bool) -> Self {
        ProcTable {
            fresh,
            ..Default::default()
        }
    }

    /// Add one row. A pid inserted twice keeps the last row.
    pub fn insert(&mut self, pid: u32, row: ProcRow) {
        if let Some(old) = self.rows.get(&pid) {
            if let Some(kids) = self.children.get_mut(&old.ppid) {
                kids.retain(|k| *k != pid);
            }
        }
        let kids = self.children.entry(row.ppid).or_default();
        kids.push(pid);
        kids.sort_unstable();
        self.rows.insert(pid, row);
    }

    pub fn row(&self, pid: u32) -> Option<&ProcRow> {
        self.rows.get(&pid)
    }

    /// `pid`'s direct children, ascending. A pid is never its own child.
    pub fn children_of(&self, pid: u32) -> Vec<u32> {
        self.children
            .get(&pid)
            .map(|kids| kids.iter().copied().filter(|k| *k != pid).collect())
            .unwrap_or_default()
    }
}

/// One process's command line, in the form its platform's chains were measured
/// in. The two are never converted into each other: a form that is not the
/// platform's own is unreadable to the matcher (UNKNOWN).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Argv {
    /// Linux: `/proc/<pid>/cmdline` split on NUL, one element per argument and
    /// every element kept, empty ones too — so `[a, "b c"]` and `[a, b, c]`,
    /// which join to the same line, stay two different argvs.
    Elements(Vec<String>),
    /// Windows: the raw command line from the process's PEB, exactly as the
    /// process was started with it.
    Line(String),
}

impl Argv {
    /// Whether this is a readable command line for `platform`'s matchers: the
    /// platform's own form, with something in it.
    fn readable_for(&self, platform: Platform) -> bool {
        match (platform, self) {
            (Platform::Linux, Argv::Elements(args)) => args.iter().any(|a| !a.is_empty()),
            (Platform::Windows, Argv::Line(line)) => !line.trim().is_empty(),
            _ => false,
        }
    }
}

/// The three answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChildClass {
    /// Read, and doing work (or matching no measured chain).
    Work,
    /// Read, and positively the session's own MCP shim (or message pump).
    ShimOrPump,
    /// Not readable well enough to say.
    Unknown,
}

impl ChildClass {
    /// The word a log line uses.
    pub fn word(self) -> &'static str {
        match self {
            ChildClass::Work => "work",
            ChildClass::ShimOrPump => "shim_or_pump",
            ChildClass::Unknown => "unknown",
        }
    }
}

/// One child's answer and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildVerdict {
    pub pid: u32,
    pub class: ChildClass,
    /// The measured chain matched, for [`ChildClass::ShimOrPump`].
    pub chain: Option<&'static str>,
    pub reason: String,
}

impl ChildVerdict {
    fn new(pid: u32, class: ChildClass, reason: impl Into<String>) -> Self {
        ChildVerdict {
            pid,
            class,
            chain: None,
            reason: reason.into(),
        }
    }
}

/// One session's answer: every direct child's verdict, and whether the clause
/// would admit the session (every child [`ChildClass::ShimOrPump`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionVerdict {
    pub claude_pid: u32,
    pub admit: bool,
    pub children: Vec<ChildVerdict>,
    /// Set when the `claude` itself could not be bound, which refuses the
    /// session with no per-child verdicts.
    pub refusal: Option<String>,
}

impl SessionVerdict {
    /// `would admit` / `would refuse`.
    pub fn word(&self) -> &'static str {
        if self.admit {
            "would admit"
        } else {
            "would refuse"
        }
    }

    /// The per-class tally, e.g. `shim_or_pump=1 work=2`, or `no children`, or
    /// `claude unbound` when [`Self::refusal`] is set.
    pub fn classes(&self) -> String {
        if self.refusal.is_some() {
            return "claude unbound".to_string();
        }
        if self.children.is_empty() {
            return "no children".to_string();
        }
        let mut tally: BTreeMap<ChildClass, usize> = BTreeMap::new();
        for child in &self.children {
            *tally.entry(child.class).or_default() += 1;
        }
        tally
            .iter()
            .map(|(class, n)| format!("{}={n}", class.word()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// PURE: an image name lowercased, path and `.exe` stripped.
fn normalize_image(image: &str) -> String {
    let base = image
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(image)
        .trim()
        .to_ascii_lowercase();
    base.strip_suffix(".exe")
        .map(str::to_string)
        .unwrap_or(base)
}

/// PURE: is `image` (normalized) a Python interpreter name: `python`, `python3`,
/// or `python3.<minor>`?
fn is_python(normalized: &str) -> bool {
    match normalized.strip_prefix("python") {
        Some("") | Some("3") => true,
        Some(rest) => rest
            .strip_prefix("3.")
            .is_some_and(|minor| !minor.is_empty() && minor.bytes().all(|b| b.is_ascii_digit())),
        None => false,
    }
}

/// A known, non-zero creation time.
fn known_start(row: &ProcRow) -> Option<u64> {
    row.start.filter(|s| *s > 0)
}

/// The subtree rooted at `root` in `table`, breadth first, root included.
/// `None` when it exceeds [`MAX_SUBTREE`] or loops.
fn subtree(table: &ProcTable, root: u32) -> Option<Vec<u32>> {
    let mut seen = BTreeSet::from([root]);
    let mut order = vec![root];
    let mut i = 0;
    while i < order.len() {
        for kid in table.children_of(order[i]) {
            if !seen.insert(kid) {
                return None;
            }
            order.push(kid);
            if order.len() > MAX_SUBTREE {
                return None;
            }
        }
        i += 1;
    }
    Some(order)
}

/// Re-confirm every member of `members` (bound from `first`) against `fresh`:
/// same creation time, same parent, and the fresh subtree has exactly the same
/// members. `Err` names the first mismatch.
fn confirm_binding(
    root: u32,
    members: &[u32],
    first: &ProcTable,
    fresh: &ProcTable,
) -> Result<(), String> {
    if !fresh.fresh {
        return Err("the re-confirming table is not a fresh read".to_string());
    }
    for &pid in members {
        let Some(then) = first.row(pid) else {
            return Err(format!("pid {pid} is not in the first table"));
        };
        let Some(start) = known_start(then) else {
            return Err(format!("pid {pid} has no known creation time"));
        };
        let Some(now) = fresh.row(pid) else {
            return Err(format!("pid {pid} left before the re-confirming read"));
        };
        if known_start(now) != Some(start) {
            return Err(format!("pid {pid} was reused (its creation time changed)"));
        }
        if now.ppid != then.ppid {
            return Err(format!("pid {pid} changed parent"));
        }
        if now.image != then.image {
            return Err(format!("pid {pid} changed image"));
        }
    }
    let Some(again) = subtree(fresh, root) else {
        return Err("the fresh subtree is unreadable (too large or cyclic)".to_string());
    };
    let a: BTreeSet<u32> = members.iter().copied().collect();
    let b: BTreeSet<u32> = again.into_iter().collect();
    if a != b {
        return Err("the subtree's membership changed between the reads".to_string());
    }
    Ok(())
}

/// PURE: split a Windows command line into arguments by the
/// `CommandLineToArgvW` rules: quotes group, `2n` backslashes before a quote are
/// `n` backslashes, `2n+1` are `n` and a literal quote, and a `""` inside quotes
/// is a literal quote. Linux never comes here: its argv arrives already split
/// ([`Argv::Elements`]).
fn split_windows_args(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_arg = false;
    let mut quoted = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            let mut n = 0;
            while i < chars.len() && chars[i] == '\\' {
                n += 1;
                i += 1;
            }
            if i < chars.len() && chars[i] == '"' {
                cur.extend(std::iter::repeat_n('\\', n / 2));
                if n % 2 == 1 {
                    cur.push('"');
                    i += 1;
                }
            } else {
                cur.extend(std::iter::repeat_n('\\', n));
            }
            in_arg = true;
            continue;
        }
        if c == '"' {
            if quoted && chars.get(i + 1) == Some(&'"') {
                cur.push('"');
                i += 2;
                continue;
            }
            quoted = !quoted;
            in_arg = true;
            i += 1;
            continue;
        }
        if !quoted && c.is_whitespace() {
            if in_arg {
                out.push(std::mem::take(&mut cur));
                in_arg = false;
            }
            i += 1;
            continue;
        }
        cur.push(c);
        in_arg = true;
        i += 1;
    }
    if in_arg {
        out.push(cur);
    }
    out
}

/// PURE: path equality — case-insensitive on Windows, exact elsewhere.
fn same_path(platform: Platform, a: &str, b: &str) -> bool {
    if platform == Platform::Windows {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// PURE: the basename of an argv[0], normalized as an image is.
fn argv0_image(arg: &str) -> String {
    normalize_image(arg)
}

/// PURE: do `args` (everything after the interpreter) run the shim exactly as
/// measured — the shim script first, then nothing or `--credential <path>`?
/// `Ok` is the script path; `Err` names the rule that refused.
fn shim_args(platform: Platform, args: &[String]) -> Result<String, &'static str> {
    let Some((script, rest)) = args.split_first() else {
        return Err("no script argument");
    };
    // Backslashes are separators on Windows only: on Linux a `\` is part of a
    // file name, and normalizing it would match a path that is not the suffix.
    let ends = if platform == Platform::Windows {
        script
            .replace('\\', "/")
            .to_ascii_lowercase()
            .ends_with(SHIM_SCRIPT_SUFFIX)
    } else {
        script.ends_with(SHIM_SCRIPT_SUFFIX)
    };
    if !ends {
        return Err("the script is not qontinui-claude-config/scripts/coord-mcp-shim.py");
    }
    match rest {
        [] => Ok(script.clone()),
        [flag, path] if flag == "--credential" && !path.is_empty() && !path.starts_with('-') => {
            Ok(script.clone())
        }
        _ => Err("the arguments after the script are neither none nor `--credential <path>`"),
    }
}

/// PURE: two argument lists equal under [`same_path`] per element.
fn same_args(platform: Platform, a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_path(platform, x, y))
}

/// What a chain matcher reads for one subtree member.
struct Member<'a> {
    image: String,
    argv: &'a Argv,
}

/// The Linux chain: `python3 <shim> [--credential <path>]`, no descendants.
/// `Err` names the rule that refused.
fn matches_linux_shim(
    child: u32,
    members: &[u32],
    view: &HashMap<u32, Member<'_>>,
) -> Result<(), &'static str> {
    if members != [child] {
        return Err("the subtree is not the shim alone");
    }
    let Some(m) = view.get(&child) else {
        return Err("the child's command line is unread");
    };
    if !is_python(&m.image) {
        return Err("the image is not a python interpreter");
    }
    let Argv::Elements(args) = m.argv else {
        return Err("the command line is not an argv");
    };
    let Some((interp, rest)) = args.split_first() else {
        return Err("the argv is empty");
    };
    if !is_python(&argv0_image(interp)) {
        return Err("argv[0] is not a python interpreter");
    }
    shim_args(Platform::Linux, rest).map(|_| ())
}

/// The Windows chain: `cmd.exe /d /s /c "<python3 shim>"` → { `conhost.exe 0x4`,
/// `python3.exe <shim>` → `python.exe <shim>` }, nothing else. `Err` names the
/// rule that refused.
fn matches_windows_cmd_shim(
    child: u32,
    table: &ProcTable,
    members: &[u32],
    view: &HashMap<u32, Member<'_>>,
) -> Result<(), &'static str> {
    const W: Platform = Platform::Windows;
    // cmd, conhost, python3 and python: the whole subtree is four members.
    if members.len() != 4 {
        return Err("the subtree does not have four members");
    }
    let Some(cmd) = view.get(&child) else {
        return Err("the child's command line is unread");
    };
    if cmd.image != "cmd" {
        return Err("the child is not cmd");
    }
    let Argv::Line(cmd_line) = cmd.argv else {
        return Err("cmd's command line is not a raw line");
    };
    let Some(inner) = cmd_inner_command(cmd_line) else {
        return Err("cmd's command line is not `cmd.exe /d /s /c \"<command>\"`");
    };
    let inner_args = split_windows_args(&inner);
    let Some((inner_interp, inner_rest)) = inner_args.split_first() else {
        return Err("cmd's command is empty");
    };
    if argv0_image(inner_interp) != "python3" {
        return Err("cmd's command does not start python3");
    }
    let script = shim_args(W, inner_rest)?;

    // cmd's two children are exactly one conhost and one python3 hop, in either
    // pid order: two of either is not the chain.
    let kids = table.children_of(child);
    let [a, b] = kids.as_slice() else {
        return Err("cmd does not have exactly two children");
    };
    let (Some(ma), Some(mb)) = (view.get(a), view.get(b)) else {
        return Err("a child of cmd is unread");
    };
    let ((conhost_pid, conhost), (py3_pid, py3)) = match (ma.image.as_str(), mb.image.as_str()) {
        ("conhost", "python3") => ((*a, ma), (*b, mb)),
        ("python3", "conhost") => ((*b, mb), (*a, ma)),
        _ => return Err("cmd's children are not one conhost and one python3"),
    };

    // conhost: childless, started as `<…\conhost.exe> 0x4`.
    let Argv::Line(conhost_line) = conhost.argv else {
        return Err("conhost's command line is not a raw line");
    };
    let conhost_args = split_windows_args(conhost_line);
    let conhost_ok = table.children_of(conhost_pid).is_empty()
        && matches!(conhost_args.as_slice(),
            [exe, flag] if argv0_image(exe) == "conhost" && flag == "0x4");
    if !conhost_ok {
        return Err("conhost is not a childless `conhost.exe 0x4`");
    }

    // python3: the WindowsApps alias hop, started with exactly cmd's command.
    let Argv::Line(py3_line) = py3.argv else {
        return Err("python3's command line is not a raw line");
    };
    if !same_args(W, &split_windows_args(py3_line), &inner_args) {
        return Err("python3's command line differs from cmd's command");
    }

    // python: the real interpreter, the hop's only child and childless, running
    // the same shim with the same arguments.
    let grandkids = table.children_of(py3_pid);
    let [real] = grandkids.as_slice() else {
        return Err("python3 does not have exactly one child");
    };
    let Some(m) = view.get(real) else {
        return Err("the real interpreter's command line is unread");
    };
    if m.image != "python" || !table.children_of(*real).is_empty() {
        return Err("the real interpreter is not a childless python");
    }
    let Argv::Line(real_line) = m.argv else {
        return Err("the real interpreter's command line is not a raw line");
    };
    let real_args = split_windows_args(real_line);
    let Some((real_interp, real_rest)) = real_args.split_first() else {
        return Err("the real interpreter's command line is empty");
    };
    if argv0_image(real_interp) != "python" {
        return Err("the real interpreter's argv[0] is not python");
    }
    let same_script = shim_args(W, real_rest).is_ok_and(|s| same_path(W, &s, &script));
    if !same_script || !same_args(W, real_rest, inner_rest) {
        return Err("the real interpreter does not run the same shim with the same arguments");
    }
    Ok(())
}

/// PURE: the command `cmd.exe /d /s /c "<command>"` runs, with `/s`'s outer
/// quotes stripped and cmd's `^` escapes removed. `None` for any other shape.
/// Every cut is made by `split_once` / `split_at_checked`, so a multibyte
/// character anywhere in the line can refuse a match but never panic.
fn cmd_inner_command(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    // argv[0]: quoted or bare.
    let (argv0, rest) = match trimmed.strip_prefix('"') {
        Some(after) => after.split_once('"')?,
        None => trimmed.split_once(char::is_whitespace)?,
    };
    if argv0_image(argv0) != "cmd" {
        return None;
    }
    let mut rest = rest.trim_start();
    for switch in ["/d", "/s", "/c"] {
        let (head, after) = rest.split_at_checked(switch.len())?;
        if !head.eq_ignore_ascii_case(switch) || !after.starts_with(char::is_whitespace) {
            return None;
        }
        rest = after.trim_start();
    }
    let body = rest.trim_end().strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c == '^' {
            out.push(chars.next()?);
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// PURE: classify one direct child of a `claude`.
///
/// `first` is the table the child was found in, `argv` the argvs read after it
/// (in each platform's [`Argv`] form; `None` when that read failed outright),
/// and `fresh` a table read after the argvs, against which every subtree member
/// is re-confirmed.
pub fn classify_child(
    platform: Platform,
    child: u32,
    first: &ProcTable,
    argv: Option<&HashMap<u32, Argv>>,
    fresh: &ProcTable,
) -> ChildVerdict {
    use ChildClass::{ShimOrPump, Unknown, Work};
    if !first.fresh {
        return ChildVerdict::new(child, Unknown, "the process table is not a fresh read");
    }
    // The image rule first. It can only ever answer Work — never admit — so it
    // needs nothing the subtree walk and the binding establish, and a busy tool
    // shell (a subtree past the walk's bound, or one changing between the
    // reads) reads as the work it is rather than as UNKNOWN. A cmd child on
    // Windows is a shell to this rule but also the measured shim wrapper, so it
    // is left to the chain below.
    let child_image = first
        .row(child)
        .and_then(|r| r.image.as_deref())
        .map(normalize_image)
        .filter(|i| !i.is_empty());
    if let Some(image) = &child_image {
        if WORK_IMAGES.contains(&image.as_str())
            && !(platform == Platform::Windows && image == "cmd")
        {
            return ChildVerdict::new(
                child,
                Work,
                format!("{image} is a tool shell or a nested agent"),
            );
        }
    }
    let Some(members) = subtree(first, child) else {
        return ChildVerdict::new(
            child,
            Unknown,
            "the subtree is unreadable (too large or cyclic)",
        );
    };
    if let Err(why) = confirm_binding(child, &members, first, fresh) {
        return ChildVerdict::new(child, Unknown, format!("binding not confirmed: {why}"));
    }
    let mut images: HashMap<u32, String> = HashMap::new();
    for &pid in &members {
        let image = first
            .row(pid)
            .and_then(|r| r.image.as_deref())
            .map(normalize_image)
            .filter(|i| !i.is_empty());
        match image {
            Some(image) => {
                images.insert(pid, image);
            }
            None => {
                return ChildVerdict::new(
                    child,
                    Unknown,
                    format!("pid {pid}'s image is unreadable"),
                )
            }
        }
    }
    // Every member's image was read above, the child's among them.
    let child_image = child_image.unwrap_or_default();
    if platform == Platform::Other {
        return ChildVerdict::new(child, Work, "no chain has been measured on this platform");
    }
    let Some(argv) = argv else {
        return ChildVerdict::new(child, Unknown, "the command-line read failed");
    };
    let mut view: HashMap<u32, Member<'_>> = HashMap::new();
    for &pid in &members {
        let Some(line) = argv.get(&pid).filter(|a| a.readable_for(platform)) else {
            return ChildVerdict::new(
                child,
                Unknown,
                format!("pid {pid}'s command line is unreadable"),
            );
        };
        view.insert(
            pid,
            Member {
                image: images.remove(&pid).unwrap_or_default(),
                argv: line,
            },
        );
    }
    let matched = match platform {
        Platform::Linux => matches_linux_shim(child, &members, &view).map(|()| CHAIN_LINUX_SHIM),
        Platform::Windows => {
            matches_windows_cmd_shim(child, first, &members, &view).map(|()| CHAIN_WINDOWS_CMD_SHIM)
        }
        Platform::Other => Err("no chain has been measured on this platform"),
    };
    match matched {
        Ok(chain) => ChildVerdict {
            pid: child,
            class: ShimOrPump,
            chain: Some(chain),
            reason: format!("whole subtree matches the measured {chain} chain"),
        },
        Err(why) if child_image == "cmd" => ChildVerdict::new(
            child,
            Work,
            format!("cmd matches no measured chain (a tool shell): {why}"),
        ),
        Err(why) => ChildVerdict::new(
            child,
            Work,
            format!(
                "{child_image} ({} member subtree) matches no measured chain: {why}",
                members.len()
            ),
        ),
    }
}

/// PURE: classify every direct child of `claude_pid`. The session is admitted
/// only when the `claude` is itself bound (same creation time and parent in both
/// tables) and every child is [`ChildClass::ShimOrPump`]; a `claude` with no
/// children is admitted by this rule alone.
pub fn classify_session(
    platform: Platform,
    claude_pid: u32,
    first: &ProcTable,
    argv: Option<&HashMap<u32, Argv>>,
    fresh: &ProcTable,
) -> SessionVerdict {
    let unbound = |why: String| SessionVerdict {
        claude_pid,
        admit: false,
        children: Vec::new(),
        refusal: Some(why),
    };
    if !first.fresh || !fresh.fresh {
        return unbound("a process table is not a fresh read".to_string());
    }
    let (Some(then), Some(now)) = (first.row(claude_pid), fresh.row(claude_pid)) else {
        return unbound(format!("claude {claude_pid} is missing from a table"));
    };
    match known_start(then) {
        Some(start) if known_start(now) == Some(start) && now.ppid == then.ppid => {}
        _ => {
            return unbound(format!(
                "claude {claude_pid} could not be bound across the reads"
            ))
        }
    }
    // A child that appeared between the reads would never be classified.
    let first_kids = first.children_of(claude_pid);
    if first_kids != fresh.children_of(claude_pid) {
        return unbound(format!(
            "claude {claude_pid}'s children changed between the reads"
        ));
    }
    let children: Vec<ChildVerdict> = first_kids
        .into_iter()
        .map(|kid| classify_child(platform, kid, first, argv, fresh))
        .collect();
    let admit = children.iter().all(|c| c.class == ChildClass::ShimOrPump);
    SessionVerdict {
        claude_pid,
        admit,
        children,
        refusal: None,
    }
}

#[cfg(test)]
mod tests;
