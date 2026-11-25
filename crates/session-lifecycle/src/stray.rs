//! `StrayEvidence`: a background tool shell proven unable to finish on its own
//! (architecture §4.1, plan
//! `2026-10-23-finished-sessions-stay-busy-behind-background-wait-loops-that-can-never-exit`
//! Phase 3), ported from `qontinui-claude-config`'s
//! `scripts/close-finished-sessions.py` (`stray_verdict`, `_stray_verdict`,
//! `split_strays`, ccfg#2057) with its rules, in one classifier — the ccfg
//! script's copy retires with the scripts.
//!
//! A shell is **stray** only when it runs ONE unbounded `until`/`while` loop
//! whose body only waits, nothing but the loop's own probes and sleeps runs
//! under it, nothing but inert commands runs before the loop, and **every part of
//! the loop condition is proven frozen** by one of three proofs:
//!
//! - **(a)** a self-matching `pgrep -f` / `ps | grep` whose pattern matches no
//!   live process once every process that itself probes the same pattern (the
//!   sibling loops and their probes) and the reader's own tree are left out;
//! - **(b-file)** a `grep`/`cat`/`tail`/`head`/`[ -f ]` on a literal absolute path
//!   no process outside the loop holds open and whose mtime is older than the
//!   idle bound (never less than 15 minutes), or a harness task's output file
//!   gone from a live `tasks/` directory;
//! - **(b-pid)** `kill -0 <pid>` where the pid is gone or was reused.
//!
//! A bounded wait (an outer `timeout`, a counter, a deadline) is class (c) and is
//! never stray. **Anything unreadable is work**: no argv, an unexpandable
//! variable, a brace expansion, a changed `IFS`, a command substitution, a
//! heredoc, a `case`, a Windows child. The verdict can only ever WITHHOLD a
//! close: a stray is still `Work` for every close conjunct (G3 holds while it
//! runs); it only lets the phase reach `Stopped` so that D19b can ask the session
//! to end its own loops. No product path kills a session's child.
//!
//! ## Decisions recorded where the source was ambiguous
//!
//! - **No regex dependency.** The ccfg script translates a POSIX pattern to a
//!   Python `re`; the kernel has empty `[dependencies]`, so [`Regex`] below is a
//!   small backtracking matcher over the same BRE/ERE subset (literals, `.`,
//!   brackets, `* + ? {m,n}`, groups, alternation, anchors, `\w \W \s \S \b \B
//!   \< \>`). A pattern outside that subset, and a match that outruns its step
//!   budget, are **not judged** and read as work — the safe direction, because a
//!   false "no match" on a pgrep pattern would manufacture a stray.
//! - **Facts, not reads.** The script reads `/proc` and the filesystem; the
//!   kernel does no I/O, so [`StrayFacts`] carries the reads (the table, start
//!   times, uids, polled files with their holders, existing directories, zombie
//!   pids) and the classifier is a pure function over it.

use std::collections::{BTreeMap, BTreeSet};

/// The loop-condition proof classes, as the script names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StrayClass {
    /// A self-matching `pgrep -f` / `ps | grep` with no live target.
    A,
    /// A frozen polled file.
    BFile,
    /// A `kill -0` on a gone or reused pid.
    BPid,
}

impl StrayClass {
    pub fn word(self) -> &'static str {
        match self {
            StrayClass::A => "a",
            StrayClass::BFile => "b-file",
            StrayClass::BPid => "b-pid",
        }
    }
}

/// The proof a tool shell is a stray wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrayEvidence {
    /// The pid of the shell.
    pub pid: u32,
    /// Each distinct proof class in the condition, sorted (`a+b-file` is two).
    pub classes: Vec<StrayClass>,
    /// Why, one clause per atom of the condition.
    pub why: String,
}

impl StrayEvidence {
    /// `a`, `b-file`, `a+b-file`, ...
    pub fn class_word(&self) -> String {
        self.classes
            .iter()
            .map(|c| c.word())
            .collect::<Vec<_>>()
            .join("+")
    }
}

/// Why a shell is NOT stray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotStray {
    /// Class (c): a bounded wait.
    pub bounded: bool,
    /// A self-matching loop whose target still runs: `waits on live pid N`.
    pub waits_on_live_pid: Option<u32>,
    pub reason: String,
}

/// The classifier's answer for one shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrayVerdict {
    Stray(StrayEvidence),
    Work(NotStray),
}

impl StrayVerdict {
    pub fn is_stray(&self) -> bool {
        matches!(self, StrayVerdict::Stray(_))
    }
    pub fn evidence(&self) -> Option<&StrayEvidence> {
        match self {
            StrayVerdict::Stray(e) => Some(e),
            StrayVerdict::Work(_) => None,
        }
    }
}

/// One process of the table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StrayProc {
    pub ppid: u32,
    /// argv as `/proc/<pid>/cmdline` separates it; empty when unreadable or
    /// freed (a zombie).
    pub argv: Vec<String>,
}

/// What a polled file read found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFacts {
    /// mtime, seconds since the epoch.
    pub mtime_secs: u64,
    /// Every process with an fd open on it (the reader's own already removed or
    /// listed in [`StrayFacts::self_pids`]). A process whose fds this user
    /// cannot list is not seen: the idle floor is the backstop.
    pub holders: BTreeSet<u32>,
    /// The content, `None` for a file too large to read.
    pub text: Option<String>,
}

/// Every read the classifier needs beyond the table (the script's `ProcFacts`).
/// A path in neither `files` nor `unreadable_files` is GONE.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StrayFacts {
    pub procs: BTreeMap<u32, StrayProc>,
    /// Start times in clock ticks, `/proc/<pid>/stat` field 22.
    pub start: BTreeMap<u32, u64>,
    /// `Some(None)` is an unreadable uid; an absent pid reads as `my_uid`.
    pub uids: BTreeMap<u32, Option<u32>>,
    pub my_uid: Option<u32>,
    pub user_names: BTreeMap<u32, String>,
    pub files: BTreeMap<String, FileFacts>,
    pub unreadable_files: BTreeSet<String>,
    /// Directories known to exist (for the harness `tasks/` exception).
    pub dirs: BTreeSet<String>,
    /// The reader's own processes (its tree and ancestors).
    pub self_pids: BTreeSet<u32>,
    /// Pids wholly exited and awaiting reap (state Z with one task left).
    pub zombies: BTreeSet<u32>,
    /// Parents read live for a process newer than the table.
    pub live_ppids: BTreeMap<u32, u32>,
    pub now_secs: u64,
}

/// Everything in [`StrayFacts`] but the table, which `classify_child` builds
/// from its own two reads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StrayContext {
    pub start: BTreeMap<u32, u64>,
    pub uids: BTreeMap<u32, Option<u32>>,
    pub my_uid: Option<u32>,
    pub user_names: BTreeMap<u32, String>,
    pub files: BTreeMap<String, FileFacts>,
    pub unreadable_files: BTreeSet<String>,
    pub dirs: BTreeSet<String>,
    pub self_pids: BTreeSet<u32>,
    pub zombies: BTreeSet<u32>,
    pub live_ppids: BTreeMap<u32, u32>,
    pub now_secs: u64,
    /// `--idle-min`, minutes; the floor [`IDLE_MIN`] applies whatever it says.
    pub idle_min: u64,
}

impl StrayContext {
    pub fn facts(&self, procs: BTreeMap<u32, StrayProc>) -> StrayFacts {
        StrayFacts {
            procs,
            start: self.start.clone(),
            uids: self.uids.clone(),
            my_uid: self.my_uid,
            user_names: self.user_names.clone(),
            files: self.files.clone(),
            unreadable_files: self.unreadable_files.clone(),
            dirs: self.dirs.clone(),
            self_pids: self.self_pids.clone(),
            zombies: self.zombies.clone(),
            live_ppids: self.live_ppids.clone(),
            now_secs: self.now_secs,
        }
    }
}

/// What a Linux tool shell sources.
const SNAPSHOT: &str = "shell-snapshots/snapshot-";
/// `--idle-min`'s default, and the floor of the frozen-file proof.
pub const IDLE_MIN: u64 = 15;
/// What a caller that could not read a command line writes in its place.
pub const UNREADABLE_CMDLINE: &str = "<command line unreadable>";
/// A polled file larger than this is not read: its wait is not judged.
pub const FILE_MAX: u64 = 16 << 20;

const SH_OPS: [&str; 19] = [
    "&&", "||", "&>", ";;", "|&", ">>", ">|", ">&", "<&", "<<", "<>", ";", "&", "|", "(", ")", "<",
    ">", "\n",
];
const SH_REDIRS: [&str; 8] = [">", ">>", ">|", "&>", ">&", "<&", "<>", "<"];
const LOOP_BODY: [&str; 5] = ["sleep", ":", "true", "echo", "printf"];
const PRE_LOOP: [&str; 6] = [":", "true", "cd", "echo", "printf", "export"];
const PROBES: [&str; 4] = ["pgrep", "grep", "egrep", "fgrep"];

// ---------------------------------------------------------------------------
// A small POSIX regex (BRE/ERE subset), because the kernel has no dependencies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassItem {
    Ch(char),
    Range(char, char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Char(char),
    Any,
    Class {
        neg: bool,
        items: Vec<ClassItem>,
    },
    Word(bool),
    Space(bool),
    Bol,
    Eol,
    WordB,
    NotWordB,
    WordStart,
    WordEnd,
    /// The previous char is not a word char (the `-w` lookbehind).
    NotAfterWord,
    /// The next char is not a word char (the `-w` lookahead).
    NotBeforeWord,
    Cat(Vec<Node>),
    Alt(Vec<Node>),
    Rep {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
    },
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// A compiled pattern with its matching mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regex {
    node: Node,
    icase: bool,
    /// Whole-line match (`-x`, `pgrep -x`).
    full: bool,
}

const FUEL: u32 = 200_000;

struct Matcher<'a> {
    text: &'a [char],
    icase: bool,
    fuel: u32,
}

impl Matcher<'_> {
    fn eq(&self, a: char, b: char) -> bool {
        if self.icase {
            a.to_lowercase().eq(b.to_lowercase())
        } else {
            a == b
        }
    }

    fn class_hit(&self, neg: bool, items: &[ClassItem], c: char) -> bool {
        let mut hit = items.iter().any(|it| match it {
            ClassItem::Ch(x) => self.eq(*x, c),
            ClassItem::Range(lo, hi) => {
                let lc: Vec<char> = c.to_lowercase().collect();
                let uc: Vec<char> = c.to_uppercase().collect();
                (*lo <= c && c <= *hi)
                    || (self.icase
                        && (lc.iter().any(|x| *lo <= *x && *x <= *hi)
                            || uc.iter().any(|x| *lo <= *x && *x <= *hi)))
            }
        });
        if neg {
            hit = !hit;
        }
        hit
    }

    /// Match `node` at `i`, calling `k` with each end position; `Err` on fuel
    /// exhaustion.
    fn m(
        &mut self,
        node: &Node,
        i: usize,
        k: &mut dyn FnMut(&mut Self, usize) -> Result<bool, ()>,
    ) -> Result<bool, ()> {
        if self.fuel == 0 {
            return Err(());
        }
        self.fuel -= 1;
        let at = |me: &Self, j: usize| me.text.get(j).copied();
        match node {
            Node::Char(c) => match at(self, i) {
                Some(t) if self.eq(*c, t) => k(self, i + 1),
                _ => Ok(false),
            },
            Node::Any => match at(self, i) {
                Some(_) => k(self, i + 1),
                None => Ok(false),
            },
            Node::Class { neg, items } => match at(self, i) {
                Some(t) if self.class_hit(*neg, items, t) => k(self, i + 1),
                _ => Ok(false),
            },
            Node::Word(want) => match at(self, i) {
                Some(t) if is_word_char(t) == *want => k(self, i + 1),
                _ => Ok(false),
            },
            Node::Space(want) => match at(self, i) {
                Some(t) if t.is_whitespace() == *want => k(self, i + 1),
                _ => Ok(false),
            },
            Node::Bol => {
                if i == 0 {
                    k(self, i)
                } else {
                    Ok(false)
                }
            }
            Node::Eol => {
                if i == self.text.len() {
                    k(self, i)
                } else {
                    Ok(false)
                }
            }
            Node::WordB | Node::NotWordB | Node::WordStart | Node::WordEnd => {
                let before = i > 0 && self.text.get(i - 1).is_some_and(|c| is_word_char(*c));
                let after = self.text.get(i).is_some_and(|c| is_word_char(*c));
                let ok = match node {
                    Node::WordB => before != after,
                    Node::NotWordB => before == after,
                    Node::WordStart => !before && after,
                    _ => before && !after,
                };
                if ok {
                    k(self, i)
                } else {
                    Ok(false)
                }
            }
            Node::NotAfterWord => {
                let before = i > 0 && self.text.get(i - 1).is_some_and(|c| is_word_char(*c));
                if before {
                    Ok(false)
                } else {
                    k(self, i)
                }
            }
            Node::NotBeforeWord => {
                let after = self.text.get(i).is_some_and(|c| is_word_char(*c));
                if after {
                    Ok(false)
                } else {
                    k(self, i)
                }
            }
            Node::Cat(parts) => self.cat(parts, i, k),
            Node::Alt(alts) => {
                for a in alts {
                    if self.m(a, i, k)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Node::Rep { node, min, max } => self.rep(node, *min, *max, 0, i, usize::MAX, k),
        }
    }

    fn cat(
        &mut self,
        parts: &[Node],
        i: usize,
        k: &mut dyn FnMut(&mut Self, usize) -> Result<bool, ()>,
    ) -> Result<bool, ()> {
        match parts.split_first() {
            None => k(self, i),
            Some((head, rest)) => self.m(head, i, &mut |me, j| me.cat(rest, j, k)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rep(
        &mut self,
        node: &Node,
        min: u32,
        max: Option<u32>,
        done: u32,
        i: usize,
        last: usize,
        k: &mut dyn FnMut(&mut Self, usize) -> Result<bool, ()>,
    ) -> Result<bool, ()> {
        // Greedy: try one more repetition first (never an empty one past the
        // minimum, which would loop), then the continuation.
        if max.is_none_or(|mx| done < mx) && !(done >= min && last == i) {
            let hit = self.m(node, i, &mut |me, j| {
                me.rep(node, min, max, done + 1, j, i, k)
            })?;
            if hit {
                return Ok(true);
            }
        }
        if done >= min {
            return k(self, i);
        }
        Ok(false)
    }
}

impl Regex {
    /// Compile a GNU BRE (`ere` false) or ERE pattern, or `None` for syntax this
    /// does not model (back-references, a backslash inside brackets, `[:class:]`).
    pub fn posix(pat: &str, ere: bool, icase: bool, full: bool) -> Option<Regex> {
        let chars: Vec<char> = pat.chars().collect();
        let mut p = ReParser {
            c: &chars,
            i: 0,
            ere,
            depth: 0,
        };
        let node = p.alt()?;
        if p.i != chars.len() {
            return None;
        }
        Some(Regex { node, icase, full })
    }

    /// A fixed string (`grep -F`).
    pub fn literal(s: &str, icase: bool, full: bool) -> Regex {
        Regex {
            node: Node::Cat(s.chars().map(Node::Char).collect()),
            icase,
            full,
        }
    }

    /// Any of several, optionally with `-w` word delimiting.
    fn any_of(alts: Vec<Regex>, icase: bool, mode: Delim) -> Regex {
        let inner = Node::Alt(alts.into_iter().map(|r| r.node).collect());
        let node = match mode {
            Delim::Word => Node::Cat(vec![Node::NotAfterWord, inner, Node::NotBeforeWord]),
            _ => inner,
        };
        Regex {
            node,
            icase,
            full: mode == Delim::Line,
        }
    }

    /// Whether the pattern matches `line` (anywhere, or whole when `full`).
    /// `Err` when the match ran out of its step budget: not judged.
    #[allow(clippy::result_unit_err)]
    pub fn is_match(&self, line: &str) -> Result<bool, ()> {
        let text: Vec<char> = line.chars().collect();
        let mut m = Matcher {
            text: &text,
            icase: self.icase,
            fuel: FUEL,
        };
        let full = self.full;
        let n = text.len();
        let start_max = if full { 0 } else { n };
        for s in 0..=start_max {
            let hit = m.m(&self.node, s, &mut |_, j| Ok(!full || j == n))?;
            if hit {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delim {
    None,
    Word,
    Line,
}

struct ReParser<'a> {
    c: &'a [char],
    i: usize,
    ere: bool,
    depth: u32,
}

impl ReParser<'_> {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }
    fn peek2(&self) -> Option<char> {
        self.c.get(self.i + 1).copied()
    }

    fn alt(&mut self) -> Option<Node> {
        let mut alts = vec![self.cat()?];
        loop {
            let is_bar = if self.ere {
                self.peek() == Some('|')
            } else {
                self.peek() == Some('\\') && self.peek2() == Some('|')
            };
            if !is_bar {
                break;
            }
            self.i += if self.ere { 1 } else { 2 };
            alts.push(self.cat()?);
        }
        Some(if alts.len() == 1 {
            alts.remove(0)
        } else {
            Node::Alt(alts)
        })
    }

    fn at_close(&self) -> bool {
        if self.ere {
            self.peek() == Some(')') && self.depth > 0
        } else {
            self.peek() == Some('\\') && self.peek2() == Some(')') && self.depth > 0
        }
    }

    fn at_bar(&self) -> bool {
        if self.ere {
            self.peek() == Some('|')
        } else {
            self.peek() == Some('\\') && self.peek2() == Some('|')
        }
    }

    fn cat(&mut self) -> Option<Node> {
        let mut parts = Vec::new();
        while self.peek().is_some() && !self.at_close() && !self.at_bar() {
            let atom = self.atom(parts.is_empty())?;
            let atom = self.quantify(atom)?;
            parts.push(atom);
        }
        Some(Node::Cat(parts))
    }

    fn quantify(&mut self, mut atom: Node) -> Option<Node> {
        loop {
            let c = self.peek();
            let (min, max, adv) = match c {
                Some('*') => (0, None, 1),
                Some('+') if self.ere => (1, None, 1),
                Some('?') if self.ere => (0, Some(1), 1),
                Some('\\') if !self.ere && self.peek2() == Some('+') => (1, None, 2),
                Some('\\') if !self.ere && self.peek2() == Some('?') => (0, Some(1), 2),
                Some('{') if self.ere => match self.interval(self.i + 1, false) {
                    Some((lo, hi, next)) => {
                        self.i = next;
                        atom = Node::Rep {
                            node: Box::new(atom),
                            min: lo,
                            max: hi,
                        };
                        continue;
                    }
                    None => return Some(atom),
                },
                Some('\\') if !self.ere && self.peek2() == Some('{') => {
                    let (lo, hi, next) = self.interval(self.i + 2, true)?;
                    self.i = next;
                    atom = Node::Rep {
                        node: Box::new(atom),
                        min: lo,
                        max: hi,
                    };
                    continue;
                }
                _ => return Some(atom),
            };
            self.i += adv;
            atom = Node::Rep {
                node: Box::new(atom),
                min,
                max,
            };
        }
    }

    /// `{m}`, `{m,}`, `{m,n}` from `from`; returns (min, max, index after).
    fn interval(&self, from: usize, bre: bool) -> Option<(u32, Option<u32>, usize)> {
        let mut j = from;
        let num = |j: &mut usize| -> Option<u32> {
            let s = *j;
            while self.c.get(*j).is_some_and(|c| c.is_ascii_digit()) {
                *j += 1;
            }
            if *j == s || *j - s > 4 {
                return None;
            }
            self.c[s..*j].iter().collect::<String>().parse().ok()
        };
        let lo = num(&mut j)?;
        let hi = if self.c.get(j) == Some(&',') {
            j += 1;
            if self.c.get(j).is_some_and(|c| c.is_ascii_digit()) {
                Some(num(&mut j)?)
            } else {
                None
            }
        } else {
            Some(lo)
        };
        if bre {
            if self.c.get(j) == Some(&'\\') && self.c.get(j + 1) == Some(&'}') {
                j += 2;
            } else {
                return None;
            }
        } else if self.c.get(j) == Some(&'}') {
            j += 1;
        } else {
            return None;
        }
        if hi.is_some_and(|h| h < lo) {
            return None;
        }
        Some((lo, hi, j))
    }

    fn atom(&mut self, first: bool) -> Option<Node> {
        let c = self.peek()?;
        self.i += 1;
        Some(match c {
            '\\' => {
                let d = self.peek()?;
                self.i += 1;
                if d.is_ascii_digit() {
                    return None;
                }
                match d {
                    'w' => Node::Word(true),
                    'W' => Node::Word(false),
                    's' => Node::Space(true),
                    'S' => Node::Space(false),
                    'b' => Node::WordB,
                    'B' => Node::NotWordB,
                    '<' => Node::WordStart,
                    '>' => Node::WordEnd,
                    '(' if !self.ere => self.group()?,
                    '{' | '}' | '|' | '+' | '?' | ')' if !self.ere => return None,
                    '.' | '[' | ']' | '(' | ')' | '*' | '+' | '?' | '{' | '}' | '|' | '^' | '$'
                    | '\\' | '/' | '-'
                        if self.ere
                            || matches!(
                                d,
                                '.' | '[' | ']' | '*' | '^' | '$' | '\\' | '/' | '-'
                            ) =>
                    {
                        Node::Char(d)
                    }
                    _ => return None,
                }
            }
            '(' if self.ere => self.group()?,
            '.' => Node::Any,
            '^' => Node::Bol,
            '$' => Node::Eol,
            '*' if first => Node::Char('*'),
            '[' => self.bracket()?,
            other => Node::Char(other),
        })
    }

    fn group(&mut self) -> Option<Node> {
        self.depth += 1;
        let inner = self.alt()?;
        self.depth -= 1;
        if self.ere {
            if self.peek() != Some(')') {
                return None;
            }
            self.i += 1;
        } else {
            if !(self.peek() == Some('\\') && self.peek2() == Some(')')) {
                return None;
            }
            self.i += 2;
        }
        Some(inner)
    }

    fn bracket(&mut self) -> Option<Node> {
        let mut neg = false;
        if self.peek() == Some('^') {
            neg = true;
            self.i += 1;
        }
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let c = self.peek()?;
            self.i += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            if c == '\\' || c == '[' {
                // A backslash inside brackets, a [:class:] or an equivalence
                // class: not modelled.
                return None;
            }
            if self.peek() == Some('-') && self.peek2().is_some_and(|d| d != ']') {
                let hi = self.peek2()?;
                if hi == '\\' || hi == '[' || hi == '-' {
                    return None;
                }
                if hi < c {
                    return None;
                }
                self.i += 2;
                items.push(ClassItem::Range(c, hi));
            } else {
                if c == '-' && !items.is_empty() && self.peek() != Some(']') {
                    return None;
                }
                items.push(ClassItem::Ch(c));
            }
        }
        Some(Node::Class { neg, items })
    }
}

// ---------------------------------------------------------------------------
// Shell lexing and parsing (the script's sh_lex and _Sh)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ShError;

#[derive(Debug, Clone, PartialEq, Eq)]
enum SegKind {
    Lit,
    Var,
    Unk,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Word {
    segs: Vec<(SegKind, String)>,
    glob: bool,
}

type Env = BTreeMap<String, Option<String>>;

impl Word {
    fn add(&mut self, kind: SegKind, text: &str) {
        if kind == SegKind::Lit {
            if let Some((SegKind::Lit, last)) = self.segs.last_mut() {
                last.push_str(text);
                return;
            }
        }
        self.segs.push((kind, text.to_string()));
    }

    /// The word's value, expanding `$NAME` from `env`; `None` when any part
    /// cannot be known or a value would word-split or glob.
    fn text(&self, env: Option<&Env>) -> Option<String> {
        if self.glob {
            return None;
        }
        let mut out = String::new();
        for (kind, t) in &self.segs {
            match kind {
                SegKind::Lit => out.push_str(t),
                SegKind::Var => {
                    let v = env.and_then(|e| e.get(t)).and_then(|v| v.as_ref())?;
                    if v.chars().any(|c| c.is_whitespace() || "*?[".contains(c)) {
                        return None;
                    }
                    out.push_str(v);
                }
                SegKind::Unk => return None,
            }
        }
        Some(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    W(Word),
    Op(&'static str),
}

fn sh_skip(c: &[char], mut i: usize, close: char) -> Result<usize, ShError> {
    let mut depth = 1;
    while i < c.len() {
        let ch = c[i];
        if ch == '\\' {
            i += 2;
            continue;
        }
        if ch == '\'' && close != '`' {
            let Some(j) = c[i + 1..].iter().position(|x| *x == '\'') else {
                return Err(ShError);
            };
            i += 1 + j + 1;
            continue;
        }
        if close == ')' && ch == '(' {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Ok(i + 1);
            }
        }
        i += 1;
    }
    Err(ShError)
}

fn is_ident(s: &str) -> bool {
    let mut it = s.chars();
    it.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && it.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn sh_dollar(c: &[char], i: usize, w: &mut Word) -> Result<usize, ShError> {
    let nxt = c.get(i + 1).copied();
    let slice = |a: usize, b: usize| c[a..b].iter().collect::<String>();
    match nxt {
        Some('\'') => Err(ShError),
        Some('(') => {
            let j = sh_skip(c, i + 2, ')')?;
            w.add(SegKind::Unk, &slice(i, j));
            Ok(j)
        }
        Some('{') => {
            let j = sh_skip(c, i + 2, '}')?;
            let name = slice(i + 2, j - 1);
            w.add(
                if is_ident(&name) {
                    SegKind::Var
                } else {
                    SegKind::Unk
                },
                &name,
            );
            Ok(j)
        }
        Some(n) if n.is_ascii_alphabetic() || n == '_' => {
            let mut j = i + 1;
            while c
                .get(j)
                .is_some_and(|x| x.is_ascii_alphanumeric() || *x == '_')
            {
                j += 1;
            }
            w.add(SegKind::Var, &slice(i + 1, j));
            Ok(j)
        }
        Some(n) if "?$!#@*-0123456789".contains(n) => {
            w.add(SegKind::Unk, &slice(i, i + 2));
            Ok(i + 2)
        }
        _ => {
            w.add(SegKind::Lit, "$");
            Ok(i + 1)
        }
    }
}

fn starts_at(c: &[char], i: usize, op: &str) -> bool {
    !op.is_empty()
        && op
            .chars()
            .enumerate()
            .all(|(k, ch)| c.get(i + k) == Some(&ch))
}

fn glob_ahead(c: &[char], i: usize) -> bool {
    let rest = &c[i..];
    match rest.first() {
        Some('*') | Some('?') => true,
        Some('[') => {
            // `\[[^\s;&|()<>]*\]`
            let mut j = 1;
            while let Some(x) = rest.get(j) {
                if *x == ']' {
                    return true;
                }
                if x.is_whitespace() || ";&|()<>".contains(*x) {
                    return false;
                }
                j += 1;
            }
            false
        }
        Some('{') => {
            // `\{[^\s;&|()<>{}]*(?:,|\.\.)[^\s;&|()<>{}]*\}`
            let mut j = 1;
            let mut sep = false;
            while let Some(x) = rest.get(j) {
                if *x == '}' {
                    return sep;
                }
                if x.is_whitespace() || ";&|()<>{".contains(*x) {
                    return false;
                }
                if *x == ',' || (*x == '.' && rest.get(j + 1) == Some(&'.')) {
                    sep = true;
                }
                j += 1;
            }
            false
        }
        _ => false,
    }
}

fn sh_lex(s: &str) -> Result<Vec<Tok>, ShError> {
    let c: Vec<char> = s.chars().collect();
    let n = c.len();
    let mut toks: Vec<Tok> = Vec::new();
    let mut w: Option<Word> = None;
    let mut i = 0;
    macro_rules! end {
        () => {
            if let Some(x) = w.take() {
                toks.push(Tok::W(x));
            }
        };
    }
    while i < n {
        let ch = c[i];
        if ch == ' ' || ch == '\t' {
            end!();
            i += 1;
        } else if ch == '\\' {
            if c.get(i + 1) != Some(&'\n') {
                let wd = w.get_or_insert_with(Word::default);
                if let Some(x) = c.get(i + 1) {
                    wd.add(SegKind::Lit, &x.to_string());
                }
            }
            i += 2;
        } else if ch == '#' && w.is_none() {
            i = c[i..].iter().position(|x| *x == '\n').map_or(n, |j| i + j);
        } else if ch == '\'' {
            let Some(j) = c[i + 1..].iter().position(|x| *x == '\'') else {
                return Err(ShError);
            };
            let wd = w.get_or_insert_with(Word::default);
            wd.add(
                SegKind::Lit,
                &c[i + 1..i + 1 + j].iter().collect::<String>(),
            );
            i += j + 2;
        } else if ch == '"' {
            let wd = w.get_or_insert_with(Word::default);
            i += 1;
            loop {
                if i >= n {
                    return Err(ShError);
                }
                let d = c[i];
                if d == '"' {
                    i += 1;
                    break;
                }
                if d == '\\' && matches!(c.get(i + 1), Some('$' | '`' | '"' | '\\' | '\n')) {
                    let e = c[i + 1];
                    wd.add(
                        SegKind::Lit,
                        &if e == '\n' {
                            String::new()
                        } else {
                            e.to_string()
                        },
                    );
                    i += 2;
                } else if d == '$' {
                    i = sh_dollar(&c, i, wd)?;
                } else if d == '`' {
                    let j = sh_skip(&c, i + 1, '`')?;
                    wd.add(SegKind::Unk, &c[i..j].iter().collect::<String>());
                    i = j;
                } else {
                    wd.add(SegKind::Lit, &d.to_string());
                    i += 1;
                }
            }
        } else if ch == '$' {
            let wd = w.get_or_insert_with(Word::default);
            i = sh_dollar(&c, i, wd)?;
        } else if ch == '`' {
            let j = sh_skip(&c, i + 1, '`')?;
            let wd = w.get_or_insert_with(Word::default);
            wd.add(SegKind::Unk, &c[i..j].iter().collect::<String>());
            i = j;
        } else {
            let op = SH_OPS.iter().find(|o| starts_at(&c, i, o)).copied();
            let Some(op) = op else {
                let glob = glob_ahead(&c, i);
                let wd = w.get_or_insert_with(Word::default);
                if glob {
                    wd.glob = true;
                }
                wd.add(SegKind::Lit, &ch.to_string());
                i += 1;
                continue;
            };
            if op == "<<" {
                return Err(ShError);
            }
            if SH_REDIRS.contains(&op) {
                if let Some(x) = &w {
                    if x.text(None)
                        .is_some_and(|t| t.chars().all(|d| d.is_ascii_digit()) && !t.is_empty())
                    {
                        w = None; // an io-number (`2>`) belongs to its redirect
                    }
                }
            }
            end!();
            toks.push(Tok::Op(op));
            i += op.chars().count();
        }
    }
    end!();
    Ok(toks)
}

type Redirs = Vec<(&'static str, Word)>;

#[derive(Debug, Clone)]
enum Cmd {
    Loop { until: bool, cond: List, body: List },
    Compound,
    Simple { words: Vec<Word>, redirs: Redirs },
}

type Pipeline = (bool, Vec<Cmd>);
type AndOr = Vec<(Option<&'static str>, Pipeline)>;
type List = Vec<(AndOr, Option<&'static str>)>;

struct ShParser {
    t: Vec<Tok>,
    i: usize,
}

impl ShParser {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn peek_op(&self, ops: &[&str]) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if ops.contains(o))
    }
    fn kw(&self, names: &[&str]) -> bool {
        match self.peek() {
            Some(Tok::W(w)) => w.text(None).is_some_and(|t| names.contains(&t.as_str())),
            _ => false,
        }
    }
    fn expect(&mut self, name: &str) -> Result<(), ShError> {
        if !self.kw(&[name]) {
            return Err(ShError);
        }
        self.i += 1;
        Ok(())
    }

    fn parse_list(&mut self, stop: &[&str]) -> Result<List, ShError> {
        let mut items = Vec::new();
        loop {
            while self.peek_op(&[";", "\n"]) {
                self.i += 1;
            }
            if self.peek().is_none() || self.peek_op(&[")"]) || self.kw(stop) {
                return Ok(items);
            }
            let ao = self.and_or()?;
            let sep = match self.peek() {
                Some(Tok::Op(o)) if [";", "&", "\n"].contains(o) => {
                    let o = *o;
                    self.i += 1;
                    Some(o)
                }
                None => None,
                Some(Tok::Op(")")) => None,
                _ if self.kw(stop) => None,
                _ => return Err(ShError),
            };
            items.push((ao, sep));
        }
    }

    fn and_or(&mut self) -> Result<AndOr, ShError> {
        let mut chain = vec![(None, self.pipeline()?)];
        while let Some(Tok::Op(o)) = self.peek() {
            let op: &'static str = match *o {
                "&&" => "&&",
                "||" => "||",
                _ => break,
            };
            self.i += 1;
            while self.peek_op(&["\n"]) {
                self.i += 1;
            }
            chain.push((Some(op), self.pipeline()?));
        }
        Ok(chain)
    }

    fn pipeline(&mut self) -> Result<Pipeline, ShError> {
        let neg = self.kw(&["!"]);
        if neg {
            self.i += 1;
        }
        let mut cmds = vec![self.command()?];
        while self.peek_op(&["|", "|&"]) {
            self.i += 1;
            while self.peek_op(&["\n"]) {
                self.i += 1;
            }
            cmds.push(self.command()?);
        }
        Ok((neg, cmds))
    }

    fn redirs(&mut self) -> Result<Redirs, ShError> {
        let mut out = Vec::new();
        while let Some(Tok::Op(o)) = self.peek() {
            let Some(op) = SH_REDIRS.iter().find(|r| *r == o).copied() else {
                break;
            };
            self.i += 1;
            let Some(Tok::W(w)) = self.peek() else {
                return Err(ShError);
            };
            out.push((op, w.clone()));
            self.i += 1;
        }
        Ok(out)
    }

    fn command(&mut self) -> Result<Cmd, ShError> {
        if self.kw(&["until", "while"]) {
            let until = self.kw(&["until"]);
            self.i += 1;
            let cond = self.parse_list(&["do"])?;
            self.expect("do")?;
            let body = self.parse_list(&["done"])?;
            self.expect("done")?;
            self.redirs()?;
            return Ok(Cmd::Loop { until, cond, body });
        }
        if self.kw(&["if"]) {
            self.i += 1;
            loop {
                self.parse_list(&["then"])?;
                self.expect("then")?;
                self.parse_list(&["elif", "else", "fi"])?;
                if self.kw(&["elif"]) {
                    self.i += 1;
                    continue;
                }
                if self.kw(&["else"]) {
                    self.i += 1;
                    self.parse_list(&["fi"])?;
                }
                self.expect("fi")?;
                self.redirs()?;
                return Ok(Cmd::Compound);
            }
        }
        if self.kw(&["for", "select"]) {
            self.i += 2;
            if self.kw(&["in"]) {
                while matches!(self.peek(), Some(Tok::W(_))) {
                    self.i += 1;
                }
            }
            while self.peek_op(&[";", "\n"]) {
                self.i += 1;
            }
            self.expect("do")?;
            self.parse_list(&["done"])?;
            self.expect("done")?;
            self.redirs()?;
            return Ok(Cmd::Compound);
        }
        if self.kw(&["{"]) {
            self.i += 1;
            self.parse_list(&["}"])?;
            self.expect("}")?;
            self.redirs()?;
            return Ok(Cmd::Compound);
        }
        if self.peek_op(&["("]) {
            self.i += 1;
            self.parse_list(&[])?;
            if !self.peek_op(&[")"]) {
                return Err(ShError);
            }
            self.i += 1;
            self.redirs()?;
            return Ok(Cmd::Compound);
        }
        if self.kw(&[
            "case", "function", "then", "do", "done", "fi", "elif", "else", "esac", "}",
        ]) {
            return Err(ShError);
        }
        let mut words = Vec::new();
        let mut redirs = Vec::new();
        loop {
            match self.peek() {
                Some(Tok::W(w)) => {
                    words.push(w.clone());
                    self.i += 1;
                }
                Some(Tok::Op(o)) if SH_REDIRS.contains(o) => {
                    redirs.extend(self.redirs()?);
                }
                _ => break,
            }
        }
        if words.is_empty() && redirs.is_empty() {
            return Err(ShError);
        }
        Ok(Cmd::Simple { words, redirs })
    }
}

fn parse_sh(s: &str) -> Option<List> {
    let toks = sh_lex(s).ok()?;
    let mut p = ShParser { t: toks, i: 0 };
    let items = p.parse_list(&[]).ok()?;
    if p.peek().is_some() {
        return None;
    }
    Some(items)
}

/// The command a Linux tool shell runs: the word Claude Code hands `eval` in
/// `bash -c "source <snapshot> ... && eval '<command>' < /dev/null && pwd -P >| <file>"`.
fn tool_command(argv: &[String]) -> Option<String> {
    let script = argv.iter().skip(1).find(|a| a.contains(SNAPSHOT))?;
    let toks = sh_lex(script).ok()?;
    for (k, tok) in toks.iter().enumerate() {
        if let (Tok::W(w), Some(Tok::W(next))) = (tok, toks.get(k + 1)) {
            if w.text(None).as_deref() == Some("eval") {
                return next.text(None);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Command models: grep, pgrep, ps
// ---------------------------------------------------------------------------

/// The image name of an argv[0], on either separator, lowercased, `.exe` dropped.
fn exe_base(path: &str) -> String {
    let b = path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    b.strip_suffix(".exe").map(str::to_string).unwrap_or(b)
}

#[derive(Debug, Clone)]
struct GrepSpec {
    pats: Vec<String>,
    mode: char,
    icase: bool,
    invert: bool,
    wordx: Option<char>,
    files: Vec<String>,
}

fn grep_spec(words: &[String]) -> Option<GrepSpec> {
    let mut pats: Vec<String> = Vec::new();
    let mut icase = false;
    let mut invert = false;
    let mut wordx = None;
    let mut pos: Vec<String> = Vec::new();
    let mut mode = match exe_base(words.first()?).as_str() {
        "egrep" => 'E',
        "fgrep" => 'F',
        _ => 'G',
    };
    let mut i = 1;
    let mut opts = true;
    while i < words.len() {
        let mut a = words[i].clone();
        i += 1;
        if !opts || a == "-" || !a.starts_with('-') {
            pos.push(a);
            continue;
        }
        if a == "--" {
            opts = false;
            continue;
        }
        if let Some(rest) = a.strip_prefix("--") {
            let (name, val) = match rest.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (rest.to_string(), None),
            };
            if [
                "quiet",
                "silent",
                "count",
                "no-messages",
                "line-number",
                "with-filename",
                "no-filename",
                "files-with-matches",
                "only-matching",
                "color",
                "colour",
                "max-count",
            ]
            .contains(&name.as_str())
            {
                if name == "max-count" && val.is_none() {
                    i += 1;
                }
                continue;
            }
            if name == "regexp" {
                if let Some(v) = val {
                    pats.push(v);
                    continue;
                }
            }
            let flag = match name.as_str() {
                "invert-match" => 'v',
                "ignore-case" => 'i',
                "word-regexp" => 'w',
                "line-regexp" => 'x',
                "extended-regexp" => 'E',
                "fixed-strings" => 'F',
                "basic-regexp" => 'G',
                _ => return None,
            };
            a = format!("-{flag}");
        }
        let flags: Vec<char> = a.chars().skip(1).collect();
        let mut k = 0;
        while k < flags.len() {
            let f = flags[k];
            if "qscnhHlob".contains(f) {
                k += 1;
                continue;
            }
            match f {
                'E' | 'F' | 'G' => mode = f,
                'i' | 'y' => icase = true,
                'v' => invert = true,
                'w' | 'x' => wordx = Some(f),
                'e' | 'm' => {
                    let tail: String = flags[k + 1..].iter().collect();
                    let val = if tail.is_empty() {
                        let v = words.get(i)?.clone();
                        i += 1;
                        v
                    } else {
                        tail
                    };
                    if f == 'e' {
                        pats.push(val);
                    }
                    break;
                }
                _ => return None, // -f, -r, -z, -P, -L, context lines ...: not modelled
            }
            k += 1;
        }
    }
    if pats.is_empty() {
        if pos.is_empty() {
            return None;
        }
        pats = vec![pos.remove(0)];
    }
    let pats = pats
        .iter()
        .flat_map(|q| q.split('\n').map(str::to_string).collect::<Vec<_>>())
        .collect();
    Some(GrepSpec {
        pats,
        mode,
        icase,
        invert,
        wordx,
        files: pos,
    })
}

/// A compiled grep line selector: line -> selected, `None` when the pattern does
/// not translate.
struct GrepMatcher {
    rx: Regex,
    invert: bool,
}

impl GrepMatcher {
    fn new(spec: &GrepSpec) -> Option<GrepMatcher> {
        let mut alts = Vec::new();
        for p in &spec.pats {
            alts.push(if spec.mode == 'F' {
                Regex::literal(p, spec.icase, false)
            } else {
                Regex::posix(p, spec.mode == 'E', spec.icase, false)?
            });
        }
        let mode = match spec.wordx {
            Some('w') => Delim::Word,
            Some('x') => Delim::Line,
            _ => Delim::None,
        };
        Some(GrepMatcher {
            rx: Regex::any_of(alts, spec.icase, mode),
            invert: spec.invert,
        })
    }
    fn selects(&self, line: &str) -> Result<bool, ()> {
        Ok(self.rx.is_match(line)? != self.invert)
    }
}

struct PgrepSpec {
    pat: String,
    icase: bool,
    exact: bool,
    no_ancestors: bool,
}

fn pgrep_spec(words: &[String]) -> Option<PgrepSpec> {
    let (mut full, mut icase, mut exact, mut anc) = (false, false, false, false);
    let mut pats: Vec<&String> = Vec::new();
    let mut i = 1;
    while i < words.len() {
        let a = &words[i];
        i += 1;
        match a.as_str() {
            "--full" => full = true,
            "--ignore-case" => icase = true,
            "--exact" => exact = true,
            "--ignore-ancestors" => anc = true,
            "--list-full" | "--list-name" | "--count" | "--newest" | "--oldest"
            | "--lightweight" => {}
            _ if a.starts_with("--delimiter") => {
                if !a.contains('=') {
                    i += 1;
                }
            }
            _ if a.starts_with('-') && a.len() > 1 => {
                let flags: Vec<char> = a.chars().skip(1).collect();
                for (k, f) in flags.iter().enumerate() {
                    match f {
                        'f' => full = true,
                        'i' => icase = true,
                        'x' => exact = true,
                        'A' => anc = true,
                        'a' | 'l' | 'c' | 'n' | 'o' | 'w' => {}
                        'd' => {
                            if k + 1 >= flags.len() {
                                i += 1;
                            }
                            break;
                        }
                        _ => return None,
                    }
                }
            }
            _ => pats.push(a),
        }
    }
    if !full || pats.len() != 1 {
        return None;
    }
    Some(PgrepSpec {
        pat: pats[0].clone(),
        icase,
        exact,
        no_ancestors: anc,
    })
}

fn all_chars(s: &str, set: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| set.contains(c))
}

/// `None` when a `ps` command does not list every process with its full command
/// line; else `Some(true)` for an args-only format (`ps -eo args`), `Some(false)`
/// for the full (`ps aux` / `ps -ef`) form.
fn ps_all(words: &[String]) -> Option<bool> {
    let (mut allp, mut fmt_only) = (false, false);
    let mut i = 1;
    while i < words.len() {
        let a = words[i].as_str();
        i += 1;
        if ["--no-headers", "--no-heading", "-w", "-ww"].contains(&a) {
            continue;
        }
        if ["--cols", "--columns", "--width", "--sort"].contains(&a) {
            i += 1;
            continue;
        }
        if ["-o", "o", "-O", "O", "--format", "-eo", "axo", "-eO"].contains(&a) {
            let fmt = words.get(i).map_or("", String::as_str);
            i += 1;
            allp = allp || ["-eo", "axo", "-eO"].contains(&a);
            let has = fmt
                .split(|c: char| !is_word_char(c))
                .any(|w| ["args", "cmd", "command"].contains(&w));
            if !has {
                return None;
            }
            let bare = fmt.trim_end_matches('=');
            fmt_only = ["args", "cmd", "command"].contains(&bare)
                && fmt.len() - bare.len() <= 1
                && !["-O", "O", "-eO"].contains(&a);
            continue;
        }
        let body = a.strip_prefix('-').unwrap_or(a);
        let bsd_all = all_chars(body, "auxwfjlvhSe")
            && body.contains('a')
            && body.contains('x')
            && !a.starts_with("-e");
        let posix_all = a.starts_with('-')
            && all_chars(a.trim_start_matches('-'), "eAfFlwjHyM")
            && a.len() > 1
            && (a.contains('e') || a.contains('A'));
        if bsd_all || posix_all {
            allp = true;
        } else {
            return None;
        }
    }
    allp.then_some(fmt_only)
}

// ---------------------------------------------------------------------------
// Facts helpers
// ---------------------------------------------------------------------------

enum FileState<'a> {
    Gone,
    Ok(&'a FileFacts, BTreeSet<u32>),
}

impl StrayFacts {
    fn argv(&self, pid: u32) -> &[String] {
        self.procs.get(&pid).map_or(&[], |p| p.argv.as_slice())
    }

    fn subtree(&self, root: u32) -> Vec<u32> {
        let mut kids: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for (pid, p) in &self.procs {
            kids.entry(p.ppid).or_default().push(*pid);
        }
        let (mut out, mut stack, mut seen) = (Vec::new(), vec![root], BTreeSet::new());
        while let Some(p) = stack.pop() {
            if !seen.insert(p) {
                continue;
            }
            if self.procs.contains_key(&p) {
                out.push(p);
            }
            if let Some(ks) = kids.get(&p) {
                stack.extend(ks.iter().copied());
            }
        }
        out
    }

    /// `me`, everything under it, and its ancestors.
    fn own_tree(&self, me: u32) -> BTreeSet<u32> {
        let mut out: BTreeSet<u32> = self.subtree(me).into_iter().collect();
        out.insert(me);
        let mut p = self.procs.get(&me).map(|x| x.ppid);
        while let Some(pp) = p {
            if !self.procs.contains_key(&pp) || out.contains(&pp) {
                break;
            }
            out.insert(pp);
            p = self.procs.get(&pp).map(|x| x.ppid);
        }
        out
    }

    fn uid(&self, pid: u32) -> Option<u32> {
        match self.uids.get(&pid) {
            Some(u) => *u,
            None => self.my_uid,
        }
    }

    fn user(&self, uid: Option<u32>) -> String {
        match uid {
            Some(u) => self
                .user_names
                .get(&u)
                .cloned()
                .unwrap_or_else(|| u.to_string()),
            None => "None".to_string(),
        }
    }

    fn file(&self, path: &str) -> Option<FileState<'_>> {
        if self.unreadable_files.contains(path) {
            return None;
        }
        match self.files.get(path) {
            None => Some(FileState::Gone),
            Some(f) => {
                let holders = f.holders.difference(&self.self_pids).copied().collect();
                Some(FileState::Ok(f, holders))
            }
        }
    }

    /// The patterns `pid` greps or pgreps for, from its own argv and any shell
    /// script it carries.
    fn probes(&self, pid: u32) -> BTreeSet<String> {
        probes_of(self.argv(pid), 0)
    }
}

fn probes_of(argv: &[String], depth: u32) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if argv
        .first()
        .is_some_and(|a| PROBES.contains(&exe_base(a).as_str()))
    {
        out.extend(probe_pats(argv));
    }
    if depth >= 3 {
        return out;
    }
    for a in argv.iter().skip(1) {
        if !(a.contains("grep") && a.chars().any(char::is_whitespace)) {
            continue;
        }
        let Ok(toks) = sh_lex(a) else { continue };
        let mut run: Vec<Option<String>> = Vec::new();
        let flush = |run: &mut Vec<Option<String>>, out: &mut BTreeSet<String>| {
            while run.first().is_some_and(|w| {
                w.as_deref().is_some_and(|w| {
                    [
                        "until", "while", "if", "elif", "then", "do", "else", "!", "{", "time",
                        "exec", "command", "nohup",
                    ]
                    .contains(&w)
                })
            }) {
                run.remove(0);
            }
            if let Some(Some(first)) = run.first() {
                if PROBES.contains(&exe_base(first).as_str()) {
                    let ws: Vec<String> =
                        run.iter().map(|w| w.clone().unwrap_or_default()).collect();
                    out.extend(probe_pats(&ws));
                }
            }
            for w in run.iter().flatten() {
                if w.contains("grep") {
                    out.extend(probes_of(&["sh".to_string(), w.clone()], depth + 1));
                }
            }
            run.clear();
        };
        for tok in &toks {
            match tok {
                Tok::W(w) => run.push(w.text(None)),
                Tok::Op(_) => flush(&mut run, &mut out),
            }
        }
        flush(&mut run, &mut out);
    }
    out
}

fn probe_pats(words: &[String]) -> BTreeSet<String> {
    if words.first().is_some_and(|w| exe_base(w) == "pgrep") {
        match pgrep_spec(words) {
            Some(s) => BTreeSet::from([s.pat]),
            None => words
                .iter()
                .skip(1)
                .filter(|w| !w.is_empty() && !w.starts_with('-'))
                .cloned()
                .collect(),
        }
    } else {
        grep_spec(words)
            .map(|s| s.pats.into_iter().collect())
            .unwrap_or_default()
    }
}

fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let lens = [8, 4, 4, 4, 12];
    parts.len() == 5
        && parts
            .iter()
            .zip(lens)
            .all(|(p, l)| p.len() == l && p.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')))
}

/// A harness task's output file: `.../claude-<uid>/<project dir>/<session
/// uuid>/tasks/<id>.output`, the project dir being the session's cwd with every
/// `/` turned into `-` (the only layout that is the harness's).
fn is_harness_task_file(path: &str) -> bool {
    let segs: Vec<&str> = path.split('/').collect();
    if segs.len() < 5 {
        return false;
    }
    let last = segs.len() - 1;
    let i = last - 4;
    let uid_seg = segs[i];
    uid_seg
        .strip_prefix("claude-")
        .is_some_and(|d| all_chars(d, "0123456789"))
        && segs[i + 1].starts_with('-')
        && is_uuid(segs[i + 2])
        && segs[i + 3] == "tasks"
        && segs[i + 4].len() > ".output".len()
        && segs[i + 4].ends_with(".output")
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn fmt_age(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, secs % 3600 / 60)
    } else {
        format!("{}m", secs / 60)
    }
}

// ---------------------------------------------------------------------------
// The classifier
// ---------------------------------------------------------------------------

/// `until`, `while`, `timeout`, ... : class (c) bounded waits.
fn word_at(c: &[char], i: usize, w: &str) -> bool {
    let n = w.chars().count();
    starts_at(c, i, w)
        && (i == 0 || !is_word_char(c[i - 1]))
        && c.get(i + n).is_none_or(|x| !is_word_char(*x))
}

fn contains_word(c: &[char], w: &str) -> bool {
    (0..c.len()).any(|i| word_at(c, i, w))
}

/// The script's `BOUNDED_RE`: an outer `timeout`, a counter or a deadline turns
/// "never" into "later".
fn is_bounded(cmd: &str) -> bool {
    let c: Vec<char> = cmd.chars().collect();
    if ["timeout", "SECONDS", "seq", "let", "break"]
        .iter()
        .any(|w| contains_word(&c, w))
    {
        return true;
    }
    if cmd.contains("((") {
        return true;
    }
    for i in 0..c.len() {
        if word_at(&c, i, "date") {
            let mut j = i + 4;
            let mut span = String::new();
            while j < c.len() && !";&|\n".contains(c[j]) {
                span.push(c[j]);
                j += 1;
            }
            if span.contains("+%s") {
                return true;
            }
        }
        if c[i].is_whitespace() && c.get(i + 1) == Some(&'-') {
            for op in ["lt", "le", "gt", "ge"] {
                if starts_at(&c, i + 2, op) && c.get(i + 4).is_none_or(|x| !is_word_char(*x)) {
                    return true;
                }
            }
        }
    }
    false
}

struct Atom {
    status: i32,
    frozen: bool,
    kind: StrayClass,
    label: String,
}

type Pipe = Vec<(Vec<String>, Vec<String>)>;

struct Classifier<'a> {
    f: &'a StrayFacts,
    idle_min: u64,
}

/// Classify one Linux tool shell. Windows command lines are never read: every
/// shell there is work (the caller does not call this).
///
/// Never panics: an input this reader trips on reads as work.
pub fn stray_verdict(facts: &StrayFacts, pid: u32, idle_min: u64) -> StrayVerdict {
    let work = |reason: &str| {
        StrayVerdict::Work(NotStray {
            bounded: false,
            waits_on_live_pid: None,
            reason: reason.to_string(),
        })
    };
    let Some(proc_) = facts.procs.get(&pid) else {
        return work("pid not in the table");
    };
    if proc_.argv.iter().any(|a| a == UNREADABLE_CMDLINE) {
        return work("command line not readable as a tool shell");
    }
    let Some(cmd) = tool_command(&proc_.argv) else {
        return work("command line not readable as a tool shell");
    };
    let has_loop_kw = {
        let c: Vec<char> = cmd.chars().collect();
        contains_word(&c, "until") || contains_word(&c, "while")
    };
    if has_loop_kw && is_bounded(&cmd) {
        return StrayVerdict::Work(NotStray {
            bounded: true,
            waits_on_live_pid: None,
            reason: "bounded wait (a timeout, counter or deadline ends it)".to_string(),
        });
    }
    let Some(items) = parse_sh(&cmd) else {
        return work("not shell this classifier reads");
    };
    Classifier { f: facts, idle_min }.verdict(pid, &proc_.argv, &items)
}

impl Classifier<'_> {
    fn verdict(&self, pid: u32, argv: &[String], items: &List) -> StrayVerdict {
        let work = |reason: String| {
            StrayVerdict::Work(NotStray {
                bounded: false,
                waits_on_live_pid: None,
                reason,
            })
        };
        // Exactly one backgroundless, un-negated, single-command until/while.
        let is_loop = |(ao, sep): &(AndOr, Option<&'static str>)| -> bool {
            if ao.len() != 1 || *sep == Some("&") {
                return false;
            }
            let (neg, cmds) = &ao[0].1;
            !*neg && cmds.len() == 1 && matches!(cmds[0], Cmd::Loop { .. })
        };
        let loops: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| is_loop(it))
            .map(|(k, _)| k)
            .collect();
        if loops.len() != 1 {
            return work("not one wait loop".to_string());
        }
        let at = loops[0];
        let mut env: Env = BTreeMap::new();
        for (ao, sep) in &items[..at] {
            if *sep == Some("&") {
                return work("starts a background job before its loop".to_string());
            }
            let single = ao.len() == 1 && ao[0].1 .1.len() == 1;
            for (_, (_, cmds)) in ao {
                for c in cmds {
                    if !pre_loop(c, &mut env, single) {
                        return work(
                            "runs more than assignments, cd and literal echo before its loop"
                                .to_string(),
                        );
                    }
                }
            }
        }
        let Cmd::Loop { until, cond, body } = &items[at].0[0].1 .1[0] else {
            return work("not one wait loop".to_string());
        };
        for (ao, _) in body {
            for (_, (_, cmds)) in ao {
                for c in cmds {
                    let Cmd::Simple { words, redirs } = c else {
                        return work("the loop body does more than wait".to_string());
                    };
                    let texts: Vec<Option<String>> = words.iter().map(|w| w.text(None)).collect();
                    let first = texts.first().and_then(|t| t.as_deref());
                    let bad_redir = redirs.iter().any(|(op, t)| {
                        let tt = t.text(None);
                        tt.as_deref() != Some("/dev/null")
                            && !(*op == ">&"
                                && tt.as_deref().is_some_and(|x| all_chars(x, "0123456789")))
                    });
                    if first.is_none_or(|f| !LOOP_BODY.contains(&f))
                        || texts.iter().any(Option::is_none)
                        || (first == Some("printf") && texts.iter().flatten().any(|t| t == "-v"))
                        || bad_redir
                    {
                        return work("the loop body does more than wait".to_string());
                    }
                }
            }
        }
        let Some(chain) = self.cond_atoms(cond, &env) else {
            return work("loop condition not readable".to_string());
        };
        let mut allowed: BTreeSet<String> = BTreeSet::from(["sleep".to_string()]);
        for (_, _, pipe) in &chain {
            for (w, _) in pipe {
                if let Some(first) = w.first() {
                    allowed.insert(exe_base(first));
                }
            }
        }
        for d in self.f.subtree(pid) {
            let dv = self.f.argv(d);
            if dv.is_empty() && self.f.zombies.contains(&d) {
                // An exited, not yet reaped child reads an EMPTY cmdline: most
                // often the loop's own probe, caught between its exit and bash's
                // wait. It runs nothing. A LIVE process with an empty cmdline
                // (one that blanked its own argv) is still work.
                continue;
            }
            let follows = exe_base(dv.first().map_or("", String::as_str)) == "tail"
                && dv.iter().skip(1).any(|a| {
                    a.starts_with("-f") || a.starts_with("-F") || a.starts_with("--follow")
                });
            let own = dv.first().is_some_and(|f| allowed.contains(&exe_base(f))) && !follows;
            if d != pid && dv != argv && !own {
                let shown: String = dv.join(" ").chars().take(60).collect();
                return work(format!("runs {shown}"));
            }
        }
        let mut status: Option<i32> = None;
        let mut parts: Vec<Atom> = Vec::new();
        for (op, neg, pipe) in &chain {
            let Some(atom) = self.atom(pid, pipe) else {
                return work("loop condition not judged".to_string());
            };
            let s = if *neg {
                i32::from(atom.status == 0)
            } else {
                atom.status
            };
            let take = match op {
                None => true,
                Some("&&") => status == Some(0),
                Some(_) => status != Some(0),
            };
            if take {
                status = Some(s);
            }
            parts.push(atom);
        }
        if let Some(live) = parts.iter().find(|a| !a.frozen) {
            let waits = live
                .label
                .strip_prefix("waits on live pid ")
                .and_then(|r| r.split(' ').next())
                .and_then(|n| n.parse().ok())
                .filter(|_| live.label.contains("self-matching loop"));
            return StrayVerdict::Work(NotStray {
                bounded: false,
                waits_on_live_pid: waits,
                reason: live.label.clone(),
            });
        }
        if (status == Some(0)) == *until {
            // `(status == 0) != (kind == while)`
            return work("its condition already holds: the loop ends on its next pass".to_string());
        }
        let classes: BTreeSet<StrayClass> = parts.iter().map(|a| a.kind).collect();
        StrayVerdict::Stray(StrayEvidence {
            pid,
            classes: classes.into_iter().collect(),
            why: parts
                .iter()
                .map(|a| a.label.clone())
                .collect::<Vec<_>>()
                .join("; "),
        })
    }

    /// The condition as `(op, negated, pipeline)` per and-or element, or `None`
    /// when it is not one and-or chain of simple commands with literal words.
    #[allow(clippy::type_complexity)]
    fn cond_atoms(
        &self,
        cond: &List,
        env: &Env,
    ) -> Option<Vec<(Option<&'static str>, bool, Pipe)>> {
        if cond.len() != 1 || cond[0].1 == Some("&") {
            return None;
        }
        let mut chain = Vec::new();
        for (op, (neg, cmds)) in &cond[0].0 {
            let mut pipe: Pipe = Vec::new();
            for c in cmds {
                let Cmd::Simple { words, redirs } = c else {
                    return None;
                };
                if words.is_empty() {
                    return None;
                }
                let texts: Option<Vec<String>> = words.iter().map(|w| w.text(Some(env))).collect();
                let texts = texts?;
                let mut inputs = Vec::new();
                for (rop, target) in redirs {
                    let t = target.text(Some(env))?;
                    if *rop == "<" {
                        inputs.push(t);
                    }
                }
                pipe.push((texts, inputs));
            }
            chain.push((*op, *neg, pipe));
        }
        Some(chain)
    }

    /// One condition pipeline -> an atom, or `None` (not modelled).
    fn atom(&self, shell: u32, pipe: &Pipe) -> Option<Atom> {
        let f = self.f;
        let (words, inputs) = pipe.first()?;
        let base = exe_base(words.first()?);
        if base == "kill"
            && pipe.len() == 1
            && words.len() == 3
            && words[1] == "-0"
            && all_chars(&words[2], "0123456789")
        {
            let pid: u32 = words[2].parse().ok()?;
            if !f.procs.contains_key(&pid) {
                return Some(Atom {
                    status: 1,
                    frozen: true,
                    kind: StrayClass::BPid,
                    label: format!("kill -0 {pid}: pid gone"),
                });
            }
            let (st, born) = (f.start.get(&pid)?, f.start.get(&shell)?);
            let status = i32::from(f.uid(pid) != f.my_uid);
            if st > born {
                return Some(Atom {
                    status,
                    frozen: true,
                    kind: StrayClass::BPid,
                    label: format!("kill -0 {pid}: pid reused by a process newer than the loop"),
                });
            }
            return Some(Atom {
                status,
                frozen: false,
                kind: StrayClass::BPid,
                label: format!("waits on live pid {pid}"),
            });
        }
        let (files, test, neg, filters): (Vec<String>, Option<String>, bool, Vec<Filter>);
        if ["[", "test", "[["].contains(&base.as_str()) && pipe.len() == 1 {
            let mut args: Vec<&String> = words.iter().skip(1).collect();
            if base != "test" {
                let close = if base == "[" { "]" } else { "]]" };
                if args.last().map(|a| a.as_str()) != Some(close) {
                    return None;
                }
                args.pop();
            }
            let n = args.first().is_some_and(|a| a.as_str() == "!");
            if n {
                args.remove(0);
            }
            if args.len() != 2 || !["-f", "-e", "-s"].contains(&args[0].as_str()) {
                return None;
            }
            files = vec![args[1].clone()];
            test = Some(args[0].clone());
            neg = n;
            filters = Vec::new();
        } else if base == "ps" || base == "pgrep" {
            return self.probe_atom(shell, pipe);
        } else if ["cat", "tail", "head"].contains(&base.as_str())
            || PROBES.contains(&base.as_str())
        {
            let mut fl: Vec<String> = inputs.clone();
            let mut flt: Vec<Filter> = Vec::new();
            if PROBES.contains(&base.as_str()) {
                let spec = grep_spec(words)?;
                fl.extend(spec.files.iter().cloned());
                flt.push(Filter::Grep(spec));
            } else {
                let rest = &words[1..];
                let (mut n, mut k) = (None::<usize>, 0);
                while k < rest.len() {
                    let a = &rest[k];
                    let digits = |s: &str| all_chars(s, "0123456789");
                    let after = |k: usize| a.chars().skip(k).collect::<String>();
                    if base != "cat" && a.starts_with("-n") && digits(&after(2)) {
                        n = after(2).parse().ok();
                    } else if base != "cat" && a.starts_with('-') && digits(&after(1)) {
                        n = after(1).parse().ok();
                    } else if base != "cat"
                        && a == "-n"
                        && rest.get(k + 1).is_some_and(|x| digits(x))
                    {
                        n = rest[k + 1].parse().ok();
                        k += 1;
                    } else if a.starts_with('-') {
                        return None; // tail -f, -c, ...: not a one-shot read
                    } else {
                        fl.push(a.clone());
                    }
                    k += 1;
                }
                flt.push(Filter::Read(base.clone(), n.unwrap_or(10)));
            }
            for (w2, in2) in &pipe[1..] {
                if !PROBES.contains(&exe_base(w2.first()?).as_str()) {
                    return None;
                }
                let spec = grep_spec(w2)?;
                if !spec.files.is_empty() || !in2.is_empty() {
                    return None;
                }
                flt.push(Filter::Grep(spec));
            }
            files = fl;
            test = None;
            neg = false;
            filters = flt;
        } else {
            return None;
        }
        if files.is_empty() {
            return None;
        }
        let (mut frozen, mut label) = (true, Vec::new());
        let mut lines: Vec<String> = Vec::new();
        let mut texts: Vec<TextState> = Vec::new();
        let floor = self.idle_min.max(IDLE_MIN) * 60;
        for path in &files {
            if !path.starts_with('/') {
                return None;
            }
            let state = f.file(path)?;
            let own: BTreeSet<u32> = f.subtree(shell).into_iter().collect();
            match state {
                FileState::Gone => {
                    texts.push(TextState::Gone);
                    // A missing file is usually one that does not exist YET; the
                    // one exception is a harness task's output file in its own
                    // session's still-present tasks/ dir.
                    if is_harness_task_file(path) && f.dirs.contains(parent_dir(path)) {
                        label.push(format!(
                            "{path} is gone from its session's tasks dir (its task ended)"
                        ));
                    } else {
                        frozen = false;
                        label.push(format!("{path} does not exist yet"));
                    }
                }
                FileState::Ok(ff, holders) => {
                    // The loop's own `grep` mid-read is no writer, even one
                    // started after the table was read.
                    let holders: BTreeSet<u32> = holders
                        .into_iter()
                        .filter(|h| {
                            !own.contains(h)
                                && !f.live_ppids.get(h).is_some_and(|pp| own.contains(pp))
                        })
                        .collect();
                    texts.push(match &ff.text {
                        Some(t) => TextState::Text(t.clone()),
                        None => TextState::Large,
                    });
                    if let Some(h) = holders.iter().next() {
                        frozen = false;
                        label.push(format!("{path} is held open by pid {h}"));
                    } else if f.now_secs.saturating_sub(ff.mtime_secs) < floor {
                        frozen = false;
                        label.push(format!(
                            "{path} changed {} ago",
                            fmt_age(f.now_secs.saturating_sub(ff.mtime_secs))
                        ));
                    } else {
                        label.push(format!(
                            "{path} unchanged for {}, no process outside the loop holds it",
                            fmt_age(f.now_secs.saturating_sub(ff.mtime_secs))
                        ));
                    }
                    // A file that passes its own test, in a loop still looping,
                    // is a wait for it to go away. Nothing need hold a lock file
                    // open and its mtime predicts nothing. A harness task's
                    // output file is the exception.
                    if let Some(t) = &test {
                        let nonempty = ff.text.as_deref() != Some("");
                        if (t != "-s" || nonempty) && !is_harness_task_file(path) {
                            return Some(Atom {
                                status: 1,
                                frozen: false,
                                kind: StrayClass::BFile,
                                label: format!(
                                    "{path} passes its test now: this waits for it to go"
                                ),
                            });
                        }
                    } else {
                        let t = ff.text.as_ref()?;
                        lines.extend(split_lines(t));
                    }
                }
            }
        }
        let status;
        if let Some(t) = &test {
            let first = texts.first()?;
            let ok = match first {
                TextState::Gone => false,
                TextState::Large => true,
                TextState::Text(s) => t != "-s" || !s.is_empty(),
            };
            status = if ok != neg { 0 } else { 1 };
        } else {
            for flt in &filters {
                match flt {
                    Filter::Read(kind, n) => {
                        lines = match kind.as_str() {
                            "cat" => lines,
                            "tail" => lines[lines.len().saturating_sub(*n)..].to_vec(),
                            _ => lines.iter().take(*n).cloned().collect(),
                        };
                    }
                    Filter::Grep(spec) => {
                        let m = GrepMatcher::new(spec)?;
                        let mut kept = Vec::new();
                        for l in &lines {
                            if m.selects(l).ok()? {
                                kept.push(l.clone());
                            }
                        }
                        lines = kept;
                    }
                }
            }
            status = i32::from(lines.is_empty());
            // A bare read that printed nothing: its status is its exit code, not
            // its output. Not modelled.
            if filters.len() == 1 && matches!(filters[0], Filter::Read(..)) && lines.is_empty() {
                return Some(Atom {
                    status: 1,
                    frozen: false,
                    kind: StrayClass::BFile,
                    label: format!(
                        "{} reads empty: a bare read's status is not known",
                        files[0]
                    ),
                });
            }
            // The same wait written as a read: it passes now, so it ends only
            // when a file changes or goes; `grep -v` waits for its matching
            // lines to go. Nothing here can rule either out.
            let inverted = filters
                .iter()
                .any(|flt| matches!(flt, Filter::Grep(s) if s.invert));
            if (status == 0 || inverted) && !files.iter().all(|p| is_harness_task_file(p)) {
                return Some(Atom {
                    status: 1,
                    frozen: false,
                    kind: StrayClass::BFile,
                    label: format!("{} can only change or go for this to end", files[0]),
                });
            }
        }
        Some(Atom {
            status,
            frozen,
            kind: StrayClass::BFile,
            label: label.join("; "),
        })
    }

    /// A `pgrep -f` or `ps | grep` condition -> a class (a) atom, or `None`.
    #[allow(clippy::type_complexity)]
    fn probe_atom(&self, shell: u32, pipe: &Pipe) -> Option<Atom> {
        let f = self.f;
        let (words, inputs) = pipe.first()?;
        if !inputs.is_empty() {
            return None;
        }
        let own = f.argv(shell).join(" ");
        let is_pgrep = exe_base(words.first()?) == "pgrep";
        // (matcher over a process line, excluded pids, line-of-pid, self-matching, patterns)
        let (hit, excluded, line, selfm, pats): (
            Box<dyn Fn(&str) -> Option<bool>>,
            BTreeSet<u32>,
            Box<dyn Fn(u32) -> String + '_>,
            bool,
            BTreeSet<String>,
        );
        if is_pgrep {
            if pipe.len() != 1 {
                return None;
            }
            let spec = pgrep_spec(words)?;
            let rx = Regex::posix(&spec.pat, true, spec.icase, spec.exact)?;
            // -A drops pgrep's ancestors: the loop shell and everything above it.
            let ex: BTreeSet<u32> = if spec.no_ancestors {
                let sub: BTreeSet<u32> = f
                    .subtree(shell)
                    .into_iter()
                    .filter(|p| *p != shell)
                    .collect();
                f.own_tree(shell).difference(&sub).copied().collect()
            } else {
                BTreeSet::new()
            };
            let sm = !ex.contains(&shell) && rx.is_match(&own).ok()?;
            hit = Box::new(move |s| rx.is_match(s).ok());
            excluded = ex;
            line = Box::new(move |p| f.argv(p).join(" "));
            selfm = sm;
            pats = BTreeSet::from([spec.pat]);
        } else {
            let args_only = ps_all(words)?;
            if pipe.len() < 2 {
                return None;
            }
            let mut matchers: Vec<GrepMatcher> = Vec::new();
            let mut ps: BTreeSet<String> = BTreeSet::new();
            for (w2, in2) in &pipe[1..] {
                if !PROBES.contains(&exe_base(w2.first()?).as_str()) {
                    return None;
                }
                let spec = grep_spec(w2)?;
                if !spec.files.is_empty() || !in2.is_empty() {
                    return None;
                }
                matchers.push(GrepMatcher::new(&spec)?);
                if !spec.invert {
                    ps.extend(spec.pats.iter().cloned());
                }
            }
            let line_of = move |p: u32| -> String {
                let cmd = f.argv(p).join(" ");
                if args_only {
                    cmd
                } else {
                    let ppid = f.procs.get(&p).map_or(0, |x| x.ppid);
                    format!("{} {} {} {}", f.user(f.uid(p)), p, ppid, cmd)
                }
            };
            let sm = {
                let l = line_of(shell);
                let mut all = true;
                for m in &matchers {
                    all &= m.selects(&l).ok()?;
                }
                all
            };
            hit = Box::new(move |s| {
                let mut all = true;
                for m in &matchers {
                    all &= m.selects(s).ok()?;
                }
                Some(all)
            });
            excluded = BTreeSet::new();
            line = Box::new(line_of);
            selfm = sm;
            pats = ps;
            if pats.is_empty() {
                return None;
            }
        }
        if !selfm {
            return Some(Atom {
                status: 1,
                frozen: false,
                kind: StrayClass::A,
                label: "not self-matching".to_string(),
            });
        }
        let mut targets: Vec<u32> = Vec::new();
        for (p, proc_) in &f.procs {
            if f.self_pids.contains(p) || excluded.contains(p) || proc_.argv.is_empty() {
                continue;
            }
            if !hit(&line(*p))? {
                continue;
            }
            if f.probes(*p).intersection(&pats).next().is_some() {
                continue;
            }
            targets.push(*p);
        }
        let shown = if is_pgrep { "pgrep" } else { "ps|grep" };
        if let Some(t) = targets.first() {
            return Some(Atom {
                status: 0,
                frozen: false,
                kind: StrayClass::A,
                label: format!(
                    "waits on live pid {t} (self-matching loop: will never exit on its own)"
                ),
            });
        }
        let first_pat = pats.iter().next().cloned().unwrap_or_default();
        Some(Atom {
            status: 0,
            frozen: true,
            kind: StrayClass::A,
            label: format!("self-matching {shown} '{first_pat}', no live target"),
        })
    }
}

enum Filter {
    Read(String, usize),
    Grep(GrepSpec),
}

enum TextState {
    Gone,
    Large,
    Text(String),
}

fn split_lines(t: &str) -> Vec<String> {
    let mut v: Vec<String> = t.split('\n').map(str::to_string).collect();
    if v.last().is_some_and(String::is_empty) {
        v.pop();
    }
    v
}

/// Whether one command before a wait loop is inert: plain assignments, `export`,
/// `cd`, `:`, `true`, or a literal `echo`/`printf` (no `-v`), with every word,
/// assigned value and redirect target readable. When `record`, the assignments
/// that persist are added to `env`; else they make those names unknown. Setting
/// `IFS` is not inert.
fn pre_loop(c: &Cmd, env: &mut Env, record: bool) -> bool {
    let Cmd::Simple { words, redirs } = c else {
        return false;
    };
    // The assignment prefix of each word: `NAME=`.
    let assign = |w: &Word| -> Option<(String, Word)> {
        let (SegKind::Lit, first) = w.segs.first()? else {
            return None;
        };
        let (name, rest) = first.split_once('=')?;
        if !is_ident(name) {
            return None;
        }
        let mut value = Word::default();
        value.add(SegKind::Lit, rest);
        for (k, t) in w.segs.iter().skip(1) {
            value.add(k.clone(), t);
        }
        value.glob = w.glob;
        Some((name.to_string(), value))
    };
    let sets: Vec<Option<(String, Word)>> = words.iter().map(assign).collect();
    let mut values: Vec<Option<String>> = Vec::new();
    for s in &sets {
        match s {
            Some((name, v)) => {
                let t = v.text(Some(env));
                if t.is_none() || name == "IFS" {
                    return false;
                }
                values.push(t);
            }
            None => values.push(None),
        }
    }
    let k = sets.iter().position(Option::is_none).unwrap_or(words.len());
    let cmd: Vec<Option<String>> = words[k..].iter().map(|w| w.text(Some(env))).collect();
    if !cmd.is_empty() {
        if cmd.iter().any(Option::is_none) {
            return false;
        }
        let first = cmd[0].as_deref().unwrap_or("");
        if !PRE_LOOP.contains(&first)
            || (first == "printf" && cmd.iter().flatten().any(|t| t == "-v"))
        {
            return false;
        }
    }
    if redirs.iter().any(|(_, t)| t.text(Some(env)).is_none()) {
        return false;
    }
    // Which assignments persist.
    let mut persist: Vec<usize> = Vec::new();
    if cmd.is_empty() {
        persist.extend(0..k);
    } else if cmd[0].as_deref() == Some("export") {
        persist.extend((k + 1..words.len()).filter(|i| sets[*i].is_some()));
    }
    for i in persist {
        if let Some((name, _)) = &sets[i] {
            env.insert(name.clone(), if record { values[i].clone() } else { None });
        }
    }
    if !cmd.is_empty() {
        // `X=v :` keeps X in a POSIX-mode shell and drops it in bash's default.
        for s in sets.iter().take(k).flatten() {
            env.insert(s.0.clone(), None);
        }
    }
    true
}

/// A tool shell's argv for the fixtures and tests: how Claude Code wraps a
/// command (`bash -c "source <snapshot> && eval '<cmd>' < /dev/null && pwd -P"`).
pub fn tool_argv(cmd: &str) -> Vec<String> {
    let quoted = format!("'{}'", cmd.replace('\'', "'\\''"));
    vec![
        "/bin/bash".to_string(),
        "-c".to_string(),
        format!(
            "source /home/u/.claude/shell-snapshots/snapshot-bash-1-x.sh 2>/dev/null || true && \
             shopt -u extglob 2>/dev/null || true && eval {quoted} < /dev/null && pwd -P >| /tmp/claude-cwd"
        ),
    ]
}

#[cfg(test)]
mod tests;
