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
//!   unreadable image or command line, or a command-line read that failed
//!   outright — none of them can admit a child.
//!
//! [`ChildClass::Work`] is a positive reading: everything needed was read and the
//! child matches no measured chain (or is a tool shell or nested `claude`, which
//! are always work). [`ChildClass::Unknown`] is the absence of a reading. Both
//! keep a session open; they are kept apart so a log can say which.
//!
//! ## The measured chains
//!
//! - **Linux** ([`CHAIN_LINUX_SHIM`]), merytshost 2026-11-08: `claude` → one
//!   `python3` running `/usr/bin/python3 …/scripts/coord-mcp-shim.py --credential
//!   <path>`, with no descendants (337 of the box's 343 `claude`s held exactly
//!   this one child; 341 of their python children ran the shim).
//! - **Windows** ([`CHAIN_WINDOWS_CMD_SHIM`]), spaceship 2026-11-08: Claude Code
//!   wraps a stdio MCP server in `cmd.exe /d /s /c "<command>"`, and the
//!   WindowsApps `python3.exe` alias adds a hop to the real interpreter, so the
//!   subtree is `cmd.exe` → { `conhost.exe 0x4`, `python3.exe <shim>` →
//!   `python.exe <shim>` } (93 of the box's 100 sessions held exactly this).
//!
//! No message-pump chain has been measured, so none is admitted; the class keeps
//! the clause's name so the pump joins it when one is.

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The Linux chain's id, as it appears in a verdict and a log line.
pub const CHAIN_LINUX_SHIM: &str = "linux-coord-mcp-shim";
/// The Windows chain's id.
pub const CHAIN_WINDOWS_CMD_SHIM: &str = "windows-cmd-coord-mcp-shim";

/// The path suffix every measured shim chain runs, compared with `/` separators.
/// The suffix is the measured location, the fleet config repo's `scripts/` —
/// not any `scripts/coord-mcp-shim.py`, so a copy elsewhere (an agent
/// worktree's, a scratch dir's) is not the measured chain.
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
    /// An opaque creation-time stamp, compared only for equality: the kernel's
    /// FILETIME on Windows, epoch seconds from `/proc` on Linux. `None` (or `0`)
    /// is UNKNOWN and binds nothing.
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

/// PURE: split a command line into arguments.
///
/// Windows follows the `CommandLineToArgvW` rules (quotes group, `2n`
/// backslashes before a quote are `n` backslashes, `2n+1` are `n` and a literal
/// quote, a `""` inside quotes is a literal quote). Linux reads a `/proc` command
/// line the runner joined with single spaces, so it splits on whitespace.
fn split_args(platform: Platform, line: &str) -> Vec<String> {
    if platform != Platform::Windows {
        return line.split_whitespace().map(str::to_string).collect();
    }
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
/// Returns the script path.
fn shim_args(platform: Platform, args: &[String]) -> Option<String> {
    let script = args.first()?;
    let normalized = script.replace('\\', "/");
    let ends = if platform == Platform::Windows {
        normalized
            .to_ascii_lowercase()
            .ends_with(SHIM_SCRIPT_SUFFIX)
    } else {
        normalized.ends_with(SHIM_SCRIPT_SUFFIX)
    };
    if !ends {
        return None;
    }
    match &args[1..] {
        [] => Some(script.clone()),
        [flag, path] if flag == "--credential" && !path.is_empty() && !path.starts_with('-') => {
            Some(script.clone())
        }
        _ => None,
    }
}

/// PURE: two argument lists equal under [`same_path`] per element.
fn same_args(platform: Platform, a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_path(platform, x, y))
}

/// What a chain matcher reads for one subtree member.
struct Member<'a> {
    image: String,
    argv: &'a str,
}

/// The Linux chain: `python3 <shim> [--credential <path>]`, no descendants.
fn matches_linux_shim(child: u32, members: &[u32], view: &HashMap<u32, Member<'_>>) -> bool {
    if members != [child] {
        return false;
    }
    let Some(m) = view.get(&child) else {
        return false;
    };
    if !is_python(&m.image) {
        return false;
    }
    let args = split_args(Platform::Linux, m.argv);
    let Some(interp) = args.first() else {
        return false;
    };
    is_python(&argv0_image(interp)) && shim_args(Platform::Linux, &args[1..]).is_some()
}

/// The Windows chain: `cmd.exe /d /s /c "<python3 shim>"` → { `conhost.exe 0x4`,
/// `python3.exe <shim>` → `python.exe <shim>` }, nothing else.
fn matches_windows_cmd_shim(
    child: u32,
    table: &ProcTable,
    members: &[u32],
    view: &HashMap<u32, Member<'_>>,
) -> bool {
    const W: Platform = Platform::Windows;
    if members.len() != 4 {
        return false;
    }
    let Some(cmd) = view.get(&child) else {
        return false;
    };
    if cmd.image != "cmd" {
        return false;
    }
    let Some(inner) = cmd_inner_command(cmd.argv) else {
        return false;
    };
    let inner_args = split_args(W, &inner);
    if inner_args.first().map(|a| argv0_image(a)).as_deref() != Some("python3") {
        return false;
    }
    let Some(script) = shim_args(W, &inner_args[1..]) else {
        return false;
    };

    let kids = table.children_of(child);
    if kids.len() != 2 {
        return false;
    }
    let mut conhost_ok = false;
    let mut python3_pid = None;
    for kid in kids {
        let Some(m) = view.get(&kid) else {
            return false;
        };
        match m.image.as_str() {
            "conhost" if !conhost_ok => {
                let args = split_args(W, m.argv);
                conhost_ok = table.children_of(kid).is_empty()
                    && args.len() == 2
                    && argv0_image(&args[0]) == "conhost"
                    && args[1] == "0x4";
                if !conhost_ok {
                    return false;
                }
            }
            "python3" if python3_pid.is_none() => {
                let args = split_args(W, m.argv);
                if !same_args(W, &args, &inner_args) {
                    return false;
                }
                python3_pid = Some(kid);
            }
            _ => return false,
        }
    }
    let Some(py3) = python3_pid else {
        return false;
    };
    if !conhost_ok {
        return false;
    }
    let grandkids = table.children_of(py3);
    let [real] = grandkids.as_slice() else {
        return false;
    };
    let Some(m) = view.get(real) else {
        return false;
    };
    if m.image != "python" || !table.children_of(*real).is_empty() {
        return false;
    }
    let args = split_args(W, m.argv);
    args.first().map(|a| argv0_image(a)).as_deref() == Some("python")
        && shim_args(W, &args[1..]).is_some_and(|s| same_path(W, &s, &script))
        && same_args(W, &args[1..], &inner_args[1..])
}

/// PURE: the command `cmd.exe /d /s /c "<command>"` runs, with `/s`'s outer
/// quotes stripped and cmd's `^` escapes removed. `None` for any other shape.
fn cmd_inner_command(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    // argv[0]: quoted or bare.
    let rest = if let Some(after) = trimmed.strip_prefix('"') {
        let end = after.find('"')?;
        if argv0_image(&after[..end]) != "cmd" {
            return None;
        }
        &after[end + 1..]
    } else {
        let end = trimmed.find(char::is_whitespace)?;
        if argv0_image(&trimmed[..end]) != "cmd" {
            return None;
        }
        &trimmed[end..]
    };
    let mut rest = rest.trim_start();
    for switch in ["/d", "/s", "/c"] {
        let head = rest.get(..switch.len())?;
        if !head.eq_ignore_ascii_case(switch) {
            return None;
        }
        let after = &rest[switch.len()..];
        if !after.starts_with(char::is_whitespace) {
            return None;
        }
        rest = after.trim_start();
    }
    let body = rest.trim_end();
    let body = body.strip_prefix('"')?.strip_suffix('"')?;
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
/// `first` is the table the child was found in, `argv` the command lines read
/// after it (`None` when that read failed outright), and `fresh` a table read
/// after the command lines, against which every subtree member is re-confirmed.
pub fn classify_child(
    platform: Platform,
    child: u32,
    first: &ProcTable,
    argv: Option<&HashMap<u32, String>>,
    fresh: &ProcTable,
) -> ChildVerdict {
    use ChildClass::{ShimOrPump, Unknown, Work};
    if !first.fresh {
        return ChildVerdict::new(child, Unknown, "the process table is not a fresh read");
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
    let child_image = images[&child].clone();
    // A cmd.exe child is a shell to the image rule, but it is also the measured
    // Windows wrapper, so only a non-Windows cmd is settled here.
    if WORK_IMAGES.contains(&child_image.as_str())
        && !(platform == Platform::Windows && child_image == "cmd")
    {
        return ChildVerdict::new(
            child,
            Work,
            format!("{child_image} is a tool shell or a nested agent"),
        );
    }
    if platform == Platform::Other {
        return ChildVerdict::new(child, Work, "no chain has been measured on this platform");
    }
    let Some(argv) = argv else {
        return ChildVerdict::new(child, Unknown, "the command-line read failed");
    };
    let mut view: HashMap<u32, Member<'_>> = HashMap::new();
    for &pid in &members {
        let Some(line) = argv.get(&pid).filter(|l| !l.trim().is_empty()) else {
            return ChildVerdict::new(
                child,
                Unknown,
                format!("pid {pid}'s command line is unreadable"),
            );
        };
        view.insert(
            pid,
            Member {
                image: images[&pid].clone(),
                argv: line,
            },
        );
    }
    let chain = match platform {
        Platform::Linux => matches_linux_shim(child, &members, &view).then_some(CHAIN_LINUX_SHIM),
        Platform::Windows => matches_windows_cmd_shim(child, first, &members, &view)
            .then_some(CHAIN_WINDOWS_CMD_SHIM),
        Platform::Other => None,
    };
    match chain {
        Some(chain) => ChildVerdict {
            pid: child,
            class: ShimOrPump,
            chain: Some(chain),
            reason: format!("whole subtree matches the measured {chain} chain"),
        },
        None if child_image == "cmd" => {
            ChildVerdict::new(child, Work, "cmd matches no measured chain (a tool shell)")
        }
        None => ChildVerdict::new(
            child,
            Work,
            format!(
                "{child_image} ({} member subtree) matches no measured chain",
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
    argv: Option<&HashMap<u32, String>>,
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
