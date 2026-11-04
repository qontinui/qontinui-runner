//! Is this tool shell work, or a wait loop that can never end?
//!
//! Plan `2026-10-23-finished-sessions-stay-busy-behind-background-wait-loops-that-can-never-exit`,
//! Phase 4: the runner's half of the rule `scripts/close-finished-sessions.py`
//! (qontinui-claude-config) applies, ported so the wind-down and the operator
//! closer agree about the same process table. `until ! pgrep -f X` reads the
//! loop's OWN `bash -c` command line, which holds `X`, so it never ends (measured
//! 2026-10-23: 15 such loops on one box, 31-48 h old, one holding a finished
//! session busy for ever). The others poll a file nothing writes any more, or a
//! pid that is gone or was reused.
//!
//! A shell is STRAY only when every part of its loop condition is proven frozen
//! and the condition still says "loop". **Anything this cannot read is work.**
//!
//! The classifier is the Python's, function for function: same names, same order,
//! and every row of that script's `stray_selftest` table is a test below. (Its
//! unreadable-table case in `end_strays_selftest` has no twin here.) The
//! deliberate differences are below. Each errs toward work unless it says otherwise:
//!
//! - **No own-tree exclusion.** The Python screener is a short-lived `python3`, so
//!   it drops its own tree from the targets and from a file's holders. The
//!   runner's own subtree holds the sessions themselves: excluding it would hide a
//!   live `git push` a loop waits on, or a live writer of a file it polls, and turn
//!   real work into a stray. So nothing is excluded here. The runner matching a
//!   pattern, or holding a polled file open, reads as a live target or holder.
//! - **`grep -w`** has no lookaround in the `regex` crate. It is written with
//!   consuming edges instead, which is the same predicate for a yes/no search.
//! - **Depth is bounded** ([`MAX_SHELL_DEPTH`]). The Python's interpreter raises
//!   `RecursionError` and reads as "not stray"; here a command line of a million
//!   `(` would overflow the stack and take the runner down with it.
//! - **A user name** comes from `/etc/passwd`, not NSS: a user only NSS knows reads
//!   as its uid, so a `ps aux | grep <user>` pattern stops matching the loop's own
//!   line and the loop reads as a real wait.
//! - **Reads are bounded and single-flight.** A loop can name any path, and a stat
//!   on a dead mount never returns; a screener that hangs is a nuisance, a runner
//!   route that hangs is an outage. Past the budget a shell reads as work. So on a
//!   box too loaded to walk its table inside the budget, a held session stays
//!   open, and the operator's closer still reaches it.
//! - **Files are read last**, after their mtime and holders: a polled log that is
//!   still changing costs a stat, and is not re-read in full on every pass.
//! - **Smaller dialect edges:** a name is ASCII; a `kill -0` pid that does not fit
//!   a `u32` is not judged; a pattern the `regex` crate refuses (`a{`) is not
//!   judged; there is no catch-all, because a panic in the blocking closure reaches
//!   the caller as a join error, which reads as "no strays".
//!
//! **What "frozen" cannot prove.** A writer this user cannot see (another uid's, a
//! container's) does not show as a holder of a polled file. That risk is the
//! plan's and the closer's alike, and it is bounded by the idle floor. Both refuse
//! the sharper case: a loop waiting for a file to be REMOVED, since nothing holds a
//! lock file open and its mtime predicts nothing.
//!
//! Linux only, like the Python: elsewhere a command line is not read, and every
//! shell stays work ([`stray_shells`] answers empty).

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

/// `{pid: (ppid, argv)}`, the whole process table: a class-(a) loop's target can
/// be anywhere on the box.
pub type ProcTable = HashMap<u32, (u32, Vec<String>)>;

/// How long a polled file must have been unchanged before it can read as frozen,
/// in minutes. The Python's `--idle-min` default.
pub const IDLE_MIN: u64 = 15;

/// How long the census may wait for a verdict. `/restart-readiness` is polled on
/// every Stop turn under a 2 s client timeout, and this path runs only for a
/// finished session one of whose children really is a wait loop.
const CENSUS_BUDGET: Duration = Duration::from_millis(1500);
/// How long a close may wait to learn which strays it owes.
const RECORD_BUDGET: Duration = Duration::from_secs(5);
/// How long a close may spend ending them: SIGTERM, 5 s, SIGKILL, 2 s, per stray.
const END_BUDGET_PER_STRAY: Duration = Duration::from_secs(10);
const END_BUDGET_MAX: Duration = Duration::from_secs(60);

/// A blocking read cannot be cancelled, so a stuck one must not be joined by
/// another. One flag per path: a poll of the census must not cost a close its record.
static CENSUS_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
/// Separate from [`END_IN_FLIGHT`]: an end still running past its budget must not
/// cost the NEXT close its record, or that close's strays outlive it unrecorded.
static RECORD_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static END_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
/// After a census that did not answer inside [`CENSUS_BUDGET`], skip it until this
/// unix second: on a box too loaded to walk `/proc` in time, paying the budget on
/// every `/restart-readiness` poll would push each answer past the client timeout.
static CENSUS_SKIP_UNTIL: AtomicU64 = AtomicU64::new(0);
const CENSUS_COOLDOWN_SECS: u64 = 30;

/// What a Linux tool shell sources.
const SNAPSHOT: &str = "shell-snapshots/snapshot-";

const SH_OPS: [&str; 19] = [
    "&&", "||", "&>", ";;", "|&", ">>", ">|", ">&", "<&", "<<", "<>", ";", "&", "|", "(", ")", "<",
    ">", "\n",
];
const SH_REDIRS: [&str; 8] = [">", ">>", ">|", "&>", ">&", "<&", "<>", "<"];
/// A body that only waits.
const LOOP_BODY: [&str; 5] = ["sleep", ":", "true", "echo", "printf"];
/// What may run before the loop. Anything else might start the writer the loop
/// waits on outside the shell's subtree (`setsid -f …`, a daemon) or change state.
const PRE_LOOP: [&str; 6] = [":", "true", "cd", "echo", "printf", "export"];
const PROBES: [&str; 4] = ["pgrep", "grep", "egrep", "fgrep"];
/// A polled file larger than this is not read: its wait is not judged.
const FILE_MAX: u64 = 16 << 20;
/// Deeper shell nesting than this is not read. See the module doc.
const MAX_SHELL_DEPTH: usize = 64;

fn re(pattern: &str) -> Regex {
    #[allow(clippy::expect_used)]
    Regex::new(pattern).expect("static regex")
}

/// A bound (an outer timeout, a counter or a deadline) turns "never" into
/// "later": class (c), never stray.
static BOUNDED_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\btimeout\b|\bSECONDS\b|\bdate\b[^;&|\n]*\+%s|\bseq\b|\s-(?:lt|le|gt|ge)\b|\$\(\(|\(\(|\blet\b|\bbreak\b",
    )
});
static LOOP_WORD_RE: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:until|while)\b"));
/// A harness task's output file: `.../claude-<uid>/<project dir>/<session
/// uuid>/tasks/<id>.output`, the project dir being the session's cwd with every
/// `/` turned into `-`. Only this layout is the harness's.
static HARNESS_TASK_FILE_RE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"/claude-\d+/-[^/]*/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/tasks/[^/]+\.output$",
    )
});
static ASSIGNMENT_RE: LazyLock<Regex> = LazyLock::new(|| re(r"^([A-Za-z_][A-Za-z0-9_]*)="));
static PS_FORMAT_NAMES_COMMAND: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:args|cmd|command)\b"));
static PS_FORMAT_ONLY_COMMAND: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:args|cmd|command)=?$"));
static PS_BSD_ALL: LazyLock<Regex> = LazyLock::new(|| re(r"^-?[auxwfjlvhSe]+$"));
static PS_UNIX_ALL: LazyLock<Regex> = LazyLock::new(|| re(r"^-[eAfFlwjHyM]+$"));
static TAIL_COUNT: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:-n\d+|-\d+)$"));

// ── the shell reader ───────────────────────────────────────────────────────────

/// Shell syntax this reader does not model.
#[derive(Debug)]
struct ShError;

#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Lit(String),
    Var(String),
    Unk,
}

/// One shell word: its parts, and whether an unquoted part holds a glob character.
#[derive(Debug, Clone, Default)]
struct Word {
    segs: Vec<Seg>,
    glob: bool,
}

/// `$NAME` values before a loop. `None` marks a name whose value is not known.
type Env = HashMap<String, Option<String>>;

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

fn splits_or_globs(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_whitespace() || matches!(c, '*' | '?' | '['))
}

impl Word {
    fn add_lit(&mut self, text: &str) {
        if let Some(Seg::Lit(last)) = self.segs.last_mut() {
            last.push_str(text);
        } else {
            self.segs.push(Seg::Lit(text.to_string()));
        }
    }

    /// The word's value, expanding `$NAME` from `env`; `None` when any part cannot
    /// be known, or when a value would word-split or glob.
    fn text(&self, env: Option<&Env>) -> Option<String> {
        if self.glob {
            return None;
        }
        let mut out = String::new();
        for seg in &self.segs {
            match seg {
                Seg::Lit(t) => out.push_str(t),
                Seg::Var(name) => {
                    let value = env.and_then(|e| e.get(name)).and_then(|v| v.as_deref())?;
                    if splits_or_globs(value) {
                        return None;
                    }
                    out.push_str(value);
                }
                Seg::Unk => return None,
            }
        }
        Some(out)
    }

    fn lit(&self) -> Option<String> {
        self.text(None)
    }
}

#[derive(Debug, Clone)]
enum Tok {
    W(Word),
    Op(&'static str),
}

fn find_char(s: &[char], from: usize, c: char) -> Option<usize> {
    s.get(from..)?
        .iter()
        .position(|&x| x == c)
        .map(|k| from + k)
}

/// The index just past the `close` that ends the `$(` / `${` / `` ` `` group opened
/// before `i`.
fn sh_skip(s: &[char], mut i: usize, close: char) -> Result<usize, ShError> {
    let mut depth = 1usize;
    while let Some(&c) = s.get(i) {
        if c == '\\' {
            i += 2;
            continue;
        }
        if c == '\'' && close != '`' {
            i = find_char(s, i + 1, '\'').ok_or(ShError)? + 1;
            continue;
        }
        if close == ')' && c == '(' {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Ok(i + 1);
            }
        }
        i += 1;
    }
    Err(ShError)
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_name(s: &[char]) -> bool {
    s.first().is_some_and(|&c| is_name_start(c)) && s.iter().all(|&c| is_name_char(c))
}

/// Lex a `$`-expansion at `s[i]` into `w`; the index after it.
fn sh_dollar(s: &[char], i: usize, w: &mut Word) -> Result<usize, ShError> {
    let nxt = s.get(i + 1).copied();
    match nxt {
        Some('\'') => Err(ShError), // ANSI-C quoting
        Some('(') => {
            let j = sh_skip(s, i + 2, ')')?;
            w.segs.push(Seg::Unk);
            Ok(j)
        }
        Some('{') => {
            let j = sh_skip(s, i + 2, '}')?;
            let name = s.get(i + 2..j - 1).ok_or(ShError)?;
            w.segs.push(if is_name(name) {
                Seg::Var(name.iter().collect())
            } else {
                Seg::Unk
            });
            Ok(j)
        }
        Some(c) if is_name_start(c) => {
            let mut j = i + 1;
            while s.get(j).is_some_and(|&c| is_name_char(c)) {
                j += 1;
            }
            let name: String = s.get(i + 1..j).ok_or(ShError)?.iter().collect();
            w.segs.push(Seg::Var(name));
            Ok(j)
        }
        Some(c) if "?$!#@*-0123456789".contains(c) => {
            w.segs.push(Seg::Unk);
            Ok(i + 2)
        }
        _ => {
            w.add_lit("$");
            Ok(i + 1)
        }
    }
}

fn is_word_stop(c: char) -> bool {
    c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '<' | '>')
}

/// `\[[^\s;&|()<>]*\]` at `s[i]`.
fn looks_like_bracket_glob(s: &[char], i: usize) -> bool {
    s.get(i + 1..)
        .unwrap_or_default()
        .iter()
        .take_while(|&&c| !is_word_stop(c))
        .any(|&c| c == ']')
}

/// `\{[^\s;&|()<>{}]*(?:,|\.\.)[^\s;&|()<>{}]*\}` at `s[i]`.
fn looks_like_brace_expansion(s: &[char], i: usize) -> bool {
    let rest = s.get(i + 1..).unwrap_or_default();
    let end = rest
        .iter()
        .position(|&c| is_word_stop(c) || c == '{' || c == '}');
    let Some(end) = end else { return false };
    if rest.get(end) != Some(&'}') {
        return false;
    }
    let body = rest.get(..end).unwrap_or_default();
    body.contains(&',') || body.windows(2).any(|p| p == ['.', '.'])
}

fn op_at(s: &[char], i: usize) -> Option<&'static str> {
    SH_OPS.iter().copied().find(|op| {
        op.chars()
            .enumerate()
            .all(|(k, c)| s.get(i + k) == Some(&c))
    })
}

/// Shell tokens of `src`. `Err` on syntax this reader does not model (a heredoc,
/// ANSI-C quoting, an open quote).
fn sh_lex(src: &str) -> Result<Vec<Tok>, ShError> {
    let s: Vec<char> = src.chars().collect();
    let n = s.len();
    let mut toks = Vec::new();
    let mut w: Option<Word> = None;
    let mut i = 0usize;
    while i < n {
        let c = s[i];
        if c == ' ' || c == '\t' {
            if let Some(done) = w.take() {
                toks.push(Tok::W(done));
            }
            i += 1;
        } else if c == '\\' {
            if s.get(i + 1) != Some(&'\n') {
                let word = w.get_or_insert_with(Word::default);
                word.add_lit(&s.get(i + 1).map(char::to_string).unwrap_or_default());
            }
            i += 2;
        } else if c == '#' && w.is_none() {
            i = find_char(&s, i, '\n').unwrap_or(n);
        } else if c == '\'' {
            let j = find_char(&s, i + 1, '\'').ok_or(ShError)?;
            let text: String = s.get(i + 1..j).ok_or(ShError)?.iter().collect();
            w.get_or_insert_with(Word::default).add_lit(&text);
            i = j + 1;
        } else if c == '"' {
            let word = w.get_or_insert_with(Word::default);
            i += 1;
            loop {
                let d = *s.get(i).ok_or(ShError)?;
                if d == '"' {
                    i += 1;
                    break;
                }
                let escaped = s.get(i + 1).copied();
                if d == '\\' && matches!(escaped, Some('$' | '`' | '"' | '\\' | '\n')) {
                    if escaped != Some('\n') {
                        word.add_lit(&escaped.map(String::from).unwrap_or_default());
                    }
                    i += 2;
                } else if d == '$' {
                    i = sh_dollar(&s, i, word)?;
                } else if d == '`' {
                    i = sh_skip(&s, i + 1, '`')?;
                    word.segs.push(Seg::Unk);
                } else {
                    word.add_lit(&d.to_string());
                    i += 1;
                }
            }
        } else if c == '$' {
            let word = w.get_or_insert_with(Word::default);
            i = sh_dollar(&s, i, word)?;
        } else if c == '`' {
            let word = w.get_or_insert_with(Word::default);
            i = sh_skip(&s, i + 1, '`')?;
            word.segs.push(Seg::Unk);
        } else if let Some(op) = op_at(&s, i) {
            if op == "<<" {
                return Err(ShError); // a heredoc
            }
            if SH_REDIRS.contains(&op)
                && w.as_ref()
                    .and_then(Word::lit)
                    .is_some_and(|t| is_digits(&t))
            {
                w = None; // an io-number (`2>`) belongs to its redirect
            }
            if let Some(done) = w.take() {
                toks.push(Tok::W(done));
            }
            toks.push(Tok::Op(op));
            i += op.len();
        } else {
            let word = w.get_or_insert_with(Word::default);
            if c == '*'
                || c == '?'
                || (c == '[' && looks_like_bracket_glob(&s, i))
                || (c == '{' && looks_like_brace_expansion(&s, i))
            {
                word.glob = true; // a glob or a brace expansion: not one known word
            }
            word.add_lit(&c.to_string());
            i += 1;
        }
    }
    if let Some(done) = w.take() {
        toks.push(Tok::W(done));
    }
    Ok(toks)
}

#[derive(Debug, Clone)]
enum Cmd {
    Loop {
        kind: String,
        cond: List,
        body: List,
    },
    Compound,
    Simple {
        words: Vec<Word>,
        redirs: Vec<(&'static str, Word)>,
    },
}

/// `(negated, commands)`.
type Pipeline = (bool, Vec<Cmd>);
/// `(operator before it, pipeline)` per link of one and-or chain.
type AndOr = Vec<(Option<&'static str>, Pipeline)>;
/// `(chain, its separator)`.
type List = Vec<(AndOr, Option<&'static str>)>;

/// A recursive-descent reader of the shell grammar a wait loop is written in:
/// lists, and-or chains, pipelines, simple commands, until/while/if/for loops,
/// `{ }` and `( )` groups. Anything else is an error.
struct Sh {
    t: Vec<Tok>,
    i: usize,
    depth: usize,
}

impl Sh {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }

    fn peek_op(&self) -> Option<&'static str> {
        match self.peek() {
            Some(Tok::Op(op)) => Some(op),
            _ => None,
        }
    }

    fn kw(&self, names: &[&str]) -> bool {
        match self.peek() {
            Some(Tok::W(w)) => w.lit().is_some_and(|t| names.contains(&t.as_str())),
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

    fn skip_newlines(&mut self) {
        while self.peek_op() == Some("\n") {
            self.i += 1;
        }
    }

    fn parse_list(&mut self, stop: &[&str]) -> Result<List, ShError> {
        let mut items = Vec::new();
        loop {
            while matches!(self.peek_op(), Some(";" | "\n")) {
                self.i += 1;
            }
            if self.peek().is_none() || self.peek_op() == Some(")") || self.kw(stop) {
                return Ok(items);
            }
            let chain = self.and_or()?;
            let sep = self.peek_op();
            if matches!(sep, Some(";" | "&" | "\n")) {
                self.i += 1;
            } else if !(self.peek().is_none() || sep == Some(")") || self.kw(stop)) {
                return Err(ShError);
            }
            items.push((chain, sep.filter(|s| matches!(*s, ";" | "&" | "\n"))));
        }
    }

    fn and_or(&mut self) -> Result<AndOr, ShError> {
        let mut chain = vec![(None, self.pipeline()?)];
        while let Some(op @ ("&&" | "||")) = self.peek_op() {
            self.i += 1;
            self.skip_newlines();
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
        while matches!(self.peek_op(), Some("|" | "|&")) {
            self.i += 1;
            self.skip_newlines();
            cmds.push(self.command()?);
        }
        Ok((neg, cmds))
    }

    fn redirs(&mut self) -> Result<Vec<(&'static str, Word)>, ShError> {
        let mut out = Vec::new();
        while let Some(op) = self.peek_op().filter(|op| SH_REDIRS.contains(op)) {
            self.i += 1;
            let Some(Tok::W(target)) = self.peek().cloned() else {
                return Err(ShError);
            };
            out.push((op, target));
            self.i += 1;
        }
        Ok(out)
    }

    fn command(&mut self) -> Result<Cmd, ShError> {
        self.depth += 1;
        if self.depth > MAX_SHELL_DEPTH {
            return Err(ShError);
        }
        let cmd = self.command_inner();
        self.depth -= 1;
        cmd
    }

    fn command_inner(&mut self) -> Result<Cmd, ShError> {
        if self.kw(&["until", "while"]) {
            let Some(Tok::W(w)) = self.peek() else {
                return Err(ShError);
            };
            let kind = w.lit().ok_or(ShError)?;
            self.i += 1;
            let cond = self.parse_list(&["do"])?;
            self.expect("do")?;
            let body = self.parse_list(&["done"])?;
            self.expect("done")?;
            self.redirs()?;
            return Ok(Cmd::Loop { kind, cond, body });
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
            while matches!(self.peek_op(), Some(";" | "\n")) {
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
        if self.peek_op() == Some("(") {
            self.i += 1;
            self.parse_list(&[])?;
            if self.peek_op() != Some(")") {
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
        let (mut words, mut redirs) = (Vec::new(), Vec::new());
        loop {
            match self.peek() {
                Some(Tok::W(w)) => {
                    words.push(w.clone());
                    self.i += 1;
                }
                Some(Tok::Op(op)) if SH_REDIRS.contains(op) => redirs.extend(self.redirs()?),
                _ => break,
            }
        }
        if words.is_empty() && redirs.is_empty() {
            return Err(ShError);
        }
        Ok(Cmd::Simple { words, redirs })
    }
}

/// The parsed list of `s`, or `None` when it is not shell this reader models.
fn parse_sh(s: &str) -> Option<List> {
    let mut p = Sh {
        t: sh_lex(s).ok()?,
        i: 0,
        depth: 0,
    };
    let items = p.parse_list(&[]).ok()?;
    p.peek().is_none().then_some(items)
}

/// The command a Linux tool shell runs: the word Claude Code hands `eval` in
/// `bash -c "source <snapshot> ... && eval '<command>' < /dev/null && pwd -P >| <file>"`.
fn tool_command(argv: &[String]) -> Option<String> {
    let script = argv.get(1..)?.iter().find(|a| a.contains(SNAPSHOT))?;
    let toks = sh_lex(script).ok()?;
    for pair in toks.windows(2) {
        if let [Tok::W(w), Tok::W(next)] = pair {
            if w.lit().as_deref() == Some("eval") {
                return next.lit();
            }
        }
    }
    None
}

// ── the process table ──────────────────────────────────────────────────────────

fn subtree(table: &ProcTable, root: u32) -> Vec<u32> {
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, (ppid, _)) in table {
        kids.entry(*ppid).or_default().push(pid);
    }
    let (mut out, mut stack, mut seen) = (Vec::new(), vec![root], HashSet::new());
    while let Some(p) = stack.pop() {
        if !seen.insert(p) {
            continue;
        }
        if table.contains_key(&p) {
            out.push(p);
        }
        if let Some(children) = kids.get(&p) {
            stack.extend(children);
        }
    }
    out
}

/// `me`, everything under it, and its ancestors.
fn own_tree(table: &ProcTable, me: u32) -> HashSet<u32> {
    let mut out: HashSet<u32> = subtree(table, me).into_iter().collect();
    out.insert(me);
    let mut p = table.get(&me).map(|(ppid, _)| *ppid);
    while let Some(pid) = p.filter(|pid| table.contains_key(pid) && !out.contains(pid)) {
        out.insert(pid);
        p = table.get(&pid).map(|(ppid, _)| *ppid);
    }
    out
}

/// The image name of an argv[0], on either separator, lowercased, `.exe` dropped.
fn exe_base(path: &str) -> String {
    let base = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_lowercase();
    base.strip_suffix(".exe")
        .map(str::to_string)
        .unwrap_or(base)
}

// ── grep, pgrep and ps ─────────────────────────────────────────────────────────

/// A `regex` crate pattern for a GNU BRE (`ere` false) or ERE pattern, or `None`
/// for syntax this does not model (bracket classes, back-references, a backslash
/// inside brackets).
fn posix_re(pat: &str, ere: bool) -> Option<String> {
    let rx = translate_posix(pat, ere)?;
    (!python_would_refuse(&rx)).then_some(rx)
}

/// Whether Python's `re`, which the operator closer's classifier runs on, would
/// refuse a translated pattern the `regex` crate compiles: an inline flag group,
/// or a repetition with nothing to repeat or stacked on another (`a**`, `^*`,
/// `a{2}{3}`, `*a`). Python reads those as "not judged", so this does too. A lazy
/// `a*?` is refused as well: work is the safe answer.
fn python_would_refuse(rx: &str) -> bool {
    let c: Vec<char> = rx.chars().collect();
    let (mut i, mut repeated, mut at_start) = (0, false, true);
    while i < c.len() {
        match c[i] {
            '\\' => {
                // A word boundary is zero-width: Python refuses a repeat on it
                // ("nothing to repeat"). `\<` and `\>` translate to `\b{start}` and
                // `\b{end}`, whose braces are part of the assertion, not a count.
                let boundary = matches!(c.get(i + 1), Some('b' | 'B'));
                i += 2;
                if boundary && c.get(i) == Some(&'{') {
                    while i < c.len() && c[i] != '}' {
                        i += 1;
                    }
                    i += 1;
                }
                (repeated, at_start) = (false, boundary);
                continue;
            }
            '[' => {
                let mut j = i + 1;
                if c.get(j) == Some(&'^') {
                    j += 1;
                }
                if c.get(j) == Some(&']') {
                    j += 1;
                }
                while j < c.len() && c[j] != ']' {
                    j += if c[j] == '\\' { 2 } else { 1 };
                }
                i = j + 1;
                (repeated, at_start) = (false, false);
                continue;
            }
            '(' => {
                if c.get(i + 1) == Some(&'?') {
                    return true;
                }
                (repeated, at_start) = (false, true);
            }
            // `$` is zero-width too: `a$*` is "nothing to repeat" in Python.
            '|' | '^' | '$' => (repeated, at_start) = (false, true),
            '*' | '+' | '?' => {
                if repeated || at_start {
                    return true;
                }
                repeated = true;
            }
            '{' => {
                if repeated || at_start {
                    return true;
                }
                while i < c.len() && c[i] != '}' {
                    i += 1;
                }
                repeated = true;
            }
            _ => (repeated, at_start) = (false, false),
        }
        i += 1;
    }
    false
}

/// [`posix_re`]'s translation step, before [`python_would_refuse`].
fn translate_posix(pat: &str, ere: bool) -> Option<String> {
    let s: Vec<char> = pat.chars().collect();
    let (mut out, mut i) = (String::new(), 0usize);
    while let Some(&c) = s.get(i) {
        if c == '\\' {
            let d = *s.get(i + 1)?;
            if d.is_ascii_digit() {
                return None;
            }
            if "wWsSbB".contains(d) {
                out.push('\\');
                out.push(d);
            } else if d == '<' {
                out.push_str(r"\b{start}");
            } else if d == '>' {
                out.push_str(r"\b{end}");
            } else if !ere && "(){}|+?".contains(d) {
                out.push(d);
            } else if ".[]()*+?{}|^$\\/-".contains(d) {
                out.push('\\');
                out.push(d);
            } else {
                return None;
            }
            i += 2;
        } else if c == '[' {
            let mut k = i + 1;
            if s.get(k) == Some(&'^') {
                k += 1;
            }
            if s.get(k) == Some(&']') {
                k += 1;
            }
            let j = find_char(&s, k, ']')?;
            let body: String = s.get(i + 1..j)?.iter().collect();
            let (neg, core) = match body.strip_prefix('^') {
                Some(rest) => (true, rest.to_string()),
                None => (false, body),
            };
            if core.contains('\\') || core.contains('[') || core.contains("--") {
                return None;
            }
            let core = core
                .replace(']', r"\]")
                .replace('&', r"\&")
                .replace('~', r"\~")
                .replace('|', r"\|");
            out.push('[');
            if neg {
                out.push('^');
            }
            out.push_str(&core);
            out.push(']');
            i = j + 1;
        } else {
            if ".*^$".contains(c) || (ere && "(){}|+?".contains(c)) {
                out.push(c);
            } else {
                out.push_str(&regex::escape(&c.to_string()));
            }
            i += 1;
        }
    }
    Some(out)
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

/// A grep command's literal words, or `None` for options this does not model.
fn grep_spec(words: &[String]) -> Option<GrepSpec> {
    let mut spec = GrepSpec {
        pats: Vec::new(),
        mode: match exe_base(words.first()?).as_str() {
            "egrep" => 'E',
            "fgrep" => 'F',
            _ => 'G',
        },
        icase: false,
        invert: false,
        wordx: None,
        files: Vec::new(),
    };
    let (mut pos, mut i, mut opts) = (Vec::new(), 1usize, true);
    while let Some(word) = words.get(i) {
        i += 1;
        if !opts || word == "-" || !word.starts_with('-') {
            pos.push(word.clone());
            continue;
        }
        if word == "--" {
            opts = false;
            continue;
        }
        let mut a = word.clone();
        if let Some(long) = word.strip_prefix("--") {
            let (name, val) = long.split_once('=').unwrap_or((long, ""));
            if matches!(
                name,
                "quiet"
                    | "silent"
                    | "count"
                    | "no-messages"
                    | "line-number"
                    | "with-filename"
                    | "no-filename"
                    | "files-with-matches"
                    | "only-matching"
                    | "color"
                    | "colour"
                    | "max-count"
            ) {
                if name == "max-count" && val.is_empty() {
                    i += 1;
                }
                continue;
            }
            if name == "regexp" && !val.is_empty() {
                spec.pats.push(val.to_string());
                continue;
            }
            let flag = match name {
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
        let letters: Vec<char> = a.chars().skip(1).collect();
        for (k, &f) in letters.iter().enumerate() {
            match f {
                'q' | 's' | 'c' | 'n' | 'h' | 'H' | 'l' | 'o' | 'b' => {}
                'E' | 'F' | 'G' => spec.mode = f,
                'i' | 'y' => spec.icase = true,
                'v' => spec.invert = true,
                'w' | 'x' => spec.wordx = Some(f),
                'e' | 'm' => {
                    let glued: String = letters.get(k + 1..).unwrap_or_default().iter().collect();
                    let val = if glued.is_empty() {
                        let next = words.get(i)?.clone();
                        i += 1;
                        next
                    } else {
                        glued
                    };
                    if f == 'e' {
                        spec.pats.push(val);
                    }
                    break;
                }
                _ => return None, // -f, -r, -z, -P, -L, context lines ...: not modelled
            }
        }
    }
    if spec.pats.is_empty() {
        if pos.is_empty() {
            return None;
        }
        spec.pats.push(pos.remove(0));
    }
    spec.pats = spec
        .pats
        .iter()
        .flat_map(|p| p.split('\n').map(str::to_string))
        .collect();
    spec.files = pos;
    Some(spec)
}

type LineMatcher = Box<dyn Fn(&str) -> bool>;

/// `line -> selected`, for a [`GrepSpec`]; `None` when the pattern does not translate.
fn grep_matcher(spec: &GrepSpec) -> Option<LineMatcher> {
    let mut parts = Vec::new();
    for p in &spec.pats {
        let r = if spec.mode == 'F' {
            regex::escape(p)
        } else {
            posix_re(p, spec.mode == 'E')?
        };
        parts.push(format!("(?:{r})"));
    }
    let rx = parts.join("|");
    let rx = match spec.wordx {
        // A yes/no search, so consuming the edge is the same predicate as a
        // lookaround; the `regex` crate has none.
        Some('w') => format!("(?:^|[^A-Za-z0-9_])(?:{rx})(?:$|[^A-Za-z0-9_])"),
        Some('x') => format!(r"\A(?:{rx})\z"),
        _ => rx,
    };
    let rx = if spec.icase { format!("(?i){rx}") } else { rx };
    let compiled = Regex::new(&rx).ok()?;
    let invert = spec.invert;
    Some(Box::new(move |line| compiled.is_match(line) != invert))
}

struct PgrepSpec {
    pat: String,
    icase: bool,
    exact: bool,
    anc: bool,
}

/// A `pgrep -f` command, or `None`: without `-f` pgrep reads process names, not
/// command lines, and the selection options (`-u`, `-P`, `-g`, ...) are not modelled.
fn pgrep_spec(words: &[String]) -> Option<PgrepSpec> {
    let (mut full, mut icase, mut exact, mut anc) = (false, false, false, false);
    let (mut pats, mut i) = (Vec::new(), 1usize);
    while let Some(a) = words.get(i) {
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
            _ if a.starts_with('-') && a.chars().count() > 1 => {
                let letters: Vec<char> = a.chars().skip(1).collect();
                for (k, &f) in letters.iter().enumerate() {
                    match f {
                        'f' => full = true,
                        'i' => icase = true,
                        'x' => exact = true,
                        'A' => anc = true,
                        'a' | 'l' | 'c' | 'n' | 'o' | 'w' => {}
                        'd' => {
                            if k + 1 == letters.len() {
                                i += 1;
                            }
                            break;
                        }
                        _ => return None,
                    }
                }
            }
            _ => pats.push(a.clone()),
        }
    }
    if !full || pats.len() != 1 {
        return None;
    }
    Some(PgrepSpec {
        pat: pats.pop()?,
        icase,
        exact,
        anc,
    })
}

#[derive(Clone, Copy, PartialEq)]
enum PsKind {
    ArgsOnly,
    Full,
}

/// `None` when a `ps` command does not list every process with its full command line.
fn ps_all(words: &[String]) -> Option<PsKind> {
    let (mut allp, mut i, mut fmt_only) = (false, 1usize, false);
    while let Some(a) = words.get(i) {
        i += 1;
        let a = a.as_str();
        if matches!(a, "--no-headers" | "--no-heading" | "-w" | "-ww") {
            continue;
        }
        if matches!(a, "--cols" | "--columns" | "--width" | "--sort") {
            i += 1;
            continue;
        }
        if matches!(
            a,
            "-o" | "o" | "-O" | "O" | "--format" | "-eo" | "axo" | "-eO"
        ) {
            let fmt = words.get(i).map(String::as_str).unwrap_or("");
            i += 1;
            allp = allp || matches!(a, "-eo" | "axo" | "-eO");
            if !PS_FORMAT_NAMES_COMMAND.is_match(fmt) {
                return None;
            }
            fmt_only = PS_FORMAT_ONLY_COMMAND.is_match(fmt) && !matches!(a, "-O" | "O" | "-eO");
            continue;
        }
        let bsd =
            PS_BSD_ALL.is_match(a) && a.contains('a') && a.contains('x') && !a.starts_with("-e");
        let unix = PS_UNIX_ALL.is_match(a) && (a.contains('e') || a.contains('A'));
        if !bsd && !unix {
            return None;
        }
        allp = true;
    }
    allp.then_some(if fmt_only {
        PsKind::ArgsOnly
    } else {
        PsKind::Full
    })
}

fn probe_pats(words: &[String]) -> HashSet<String> {
    let Some(first) = words.first() else {
        return HashSet::new();
    };
    if exe_base(first) == "pgrep" {
        return match pgrep_spec(words) {
            Some(spec) => HashSet::from([spec.pat]),
            None => words
                .iter()
                .skip(1)
                .filter(|w| !w.is_empty() && !w.starts_with('-'))
                .cloned()
                .collect(),
        };
    }
    grep_spec(words)
        .map(|spec| spec.pats.into_iter().collect())
        .unwrap_or_default()
}

/// The patterns a process greps or pgreps for: its own when it IS pgrep or grep,
/// plus those of every pgrep/grep inside a shell script it carries (`bash -c`,
/// `eval`, a watcher's `--probe`), three levels deep.
fn probes_of(argv: &[String], depth: usize) -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(first) = argv.first() else {
        return out;
    };
    if PROBES.contains(&exe_base(first).as_str()) {
        out.extend(probe_pats(argv));
    }
    if depth >= 3 {
        return out;
    }
    for a in argv.iter().skip(1) {
        if !(a.contains("grep") && a.chars().any(char::is_whitespace)) {
            continue;
        }
        let Ok(mut toks) = sh_lex(a) else { continue };
        toks.push(Tok::Op(";"));
        let mut run: Vec<Option<String>> = Vec::new();
        for tok in toks {
            match tok {
                Tok::W(w) => run.push(w.lit()),
                Tok::Op(_) => {
                    while run.first().is_some_and(|w| {
                        w.as_deref().is_some_and(|t| {
                            matches!(
                                t,
                                "until"
                                    | "while"
                                    | "if"
                                    | "elif"
                                    | "then"
                                    | "do"
                                    | "else"
                                    | "!"
                                    | "{"
                                    | "time"
                                    | "exec"
                                    | "command"
                                    | "nohup"
                            )
                        })
                    }) {
                        run.remove(0);
                    }
                    if let Some(Some(head)) = run.first() {
                        if PROBES.contains(&exe_base(head).as_str()) {
                            let words: Vec<String> =
                                run.iter().map(|w| w.clone().unwrap_or_default()).collect();
                            out.extend(probe_pats(&words));
                        }
                    }
                    for w in run.iter().flatten() {
                        if w.contains("grep") {
                            out.extend(probes_of(&["sh".to_string(), w.clone()], depth + 1));
                        }
                    }
                    run.clear();
                }
            }
        }
    }
    out
}

// ── what the classifier reads beyond the table ─────────────────────────────────

/// One polled file, by `stat` alone.
#[derive(Debug, Clone, Copy)]
pub enum FileStat {
    Gone,
    /// Unix seconds.
    Present(f64),
}

/// The reads the classifier needs beyond the `(ppid, argv)` table. The tests pass
/// fixed answers and read nothing live.
pub trait Facts {
    /// The process's start time, in any unit that orders start times.
    fn start(&self, pid: u32) -> Option<i64>;
    fn uid(&self, pid: u32) -> Option<u32>;
    /// This process's uid.
    fn my_uid(&self) -> Option<u32>;
    fn user_name(&self, uid: Option<u32>) -> String;
    fn isdir(&self, path: &str) -> bool;
    /// `None` when unreadable, or not a regular file.
    fn stat(&self, path: &str) -> Option<FileStat>;
    /// Every process with an fd open on it.
    fn holders(&self, path: &str) -> HashSet<u32>;
    /// `None` when unreadable; `Some(None)` for a file too large to read.
    fn text(&self, path: &str) -> Option<Option<String>>;
    /// Unix seconds.
    fn now(&self) -> f64;
}

/// `(status, frozen, kind, label)`: the exit status a condition pipeline has now,
/// and whether that can never change.
type Atom = (i32, bool, &'static str, String);
/// One pipeline of a condition: `(words, input files)` per command.
type CondPipe = Vec<(Vec<String>, Vec<String>)>;

fn fmt_age(seconds: f64) -> String {
    let minutes = (seconds.max(0.0) / 60.0) as u64;
    if minutes >= 60 {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

/// Whether one command before a wait loop is inert: plain assignments, `export`,
/// `cd`, `:`, `true`, or a literal `echo`/`printf` (no `-v`), with every word,
/// assigned value and redirect target readable (a value can hide a command
/// substitution). When `record`, the assignments that persist are added to `env`;
/// else they make those names unknown. Setting IFS is not inert.
fn pre_loop(c: &Cmd, env: &mut Env, record: bool) -> bool {
    let Cmd::Simple { words, redirs } = c else {
        return false;
    };
    let mut sets: Vec<Option<(String, Option<String>)>> = Vec::with_capacity(words.len());
    for w in words {
        let assignment = match w.segs.first() {
            Some(Seg::Lit(first)) => ASSIGNMENT_RE
                .captures(first)
                .and_then(|m| Some((m.get(1)?.as_str().to_string(), m.get(0)?.end()))),
            _ => None,
        };
        let Some((name, end)) = assignment else {
            sets.push(None);
            continue;
        };
        let mut value = Word {
            segs: Vec::new(),
            glob: w.glob,
        };
        for (k, seg) in w.segs.iter().enumerate() {
            match seg {
                Seg::Lit(t) if k == 0 => value.add_lit(t.get(end..).unwrap_or("")),
                Seg::Lit(t) => value.add_lit(t),
                other => value.segs.push(other.clone()),
            }
        }
        let text = value.text(Some(env));
        if text.is_none() || name == "IFS" {
            return false;
        }
        sets.push(Some((name, text)));
    }
    let k = sets.iter().position(Option::is_none).unwrap_or(words.len());
    let cmd: Vec<Option<String>> = words.iter().skip(k).map(|w| w.text(Some(env))).collect();
    if let Some(head) = cmd.first() {
        let Some(head) = head else { return false };
        if cmd.iter().any(Option::is_none)
            || !PRE_LOOP.contains(&head.as_str())
            || (head == "printf" && cmd.iter().flatten().any(|w| w == "-v"))
        {
            return false;
        }
    }
    if redirs.iter().any(|(_, t)| t.text(Some(env)).is_none()) {
        return false;
    }
    let head = cmd.first().and_then(|h| h.as_deref());
    let persist: Vec<&(String, Option<String>)> = match head {
        None => sets.iter().take(k).flatten().collect(),
        Some("export") => sets.iter().skip(k + 1).flatten().collect(),
        Some(_) => Vec::new(),
    };
    let persisted: Vec<(String, Option<String>)> = persist.into_iter().cloned().collect();
    for (name, value) in persisted {
        env.insert(name, if record { value } else { None });
    }
    if head.is_some() {
        // `X=v :` keeps X in a POSIX-mode shell and drops it in bash's default mode
        for (name, _) in sets.iter().take(k).flatten() {
            env.insert(name.clone(), None);
        }
    }
    true
}

/// A loop's condition as `(operator, negated, pipeline)` per link, or `None` when
/// it is not one and-or chain of simple commands with literal words.
fn cond_atoms(cond: &List, env: &Env) -> Option<Vec<(Option<&'static str>, bool, CondPipe)>> {
    let [(chain_in, sep)] = cond.as_slice() else {
        return None;
    };
    if *sep == Some("&") {
        return None;
    }
    let mut chain = Vec::new();
    for (op, (neg, cmds)) in chain_in {
        let mut pipe = Vec::new();
        for c in cmds {
            let Cmd::Simple { words, redirs } = c else {
                return None;
            };
            if words.is_empty() {
                return None;
            }
            let words: Option<Vec<String>> = words.iter().map(|w| w.text(Some(env))).collect();
            let mut inputs = Vec::new();
            for (rop, target) in redirs {
                let t = target.text(Some(env))?;
                if *rop == "<" {
                    inputs.push(t);
                }
            }
            pipe.push((words?, inputs));
        }
        chain.push((*op, *neg, pipe));
    }
    Some(chain)
}

fn split_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

fn joined_argv(table: &ProcTable, pid: u32) -> String {
    table
        .get(&pid)
        .map(|(_, argv)| argv.join(" "))
        .unwrap_or_default()
}

/// One condition pipeline, or `None` when it is not modelled.
fn atom(
    table: &ProcTable,
    shell: u32,
    pipe: &CondPipe,
    facts: &dyn Facts,
    idle_min: u64,
) -> Option<Atom> {
    let (words, inputs) = pipe.first()?;
    let base = exe_base(words.first()?);
    if base == "kill"
        && pipe.len() == 1
        && words.len() == 3
        && words[1] == "-0"
        && is_digits(&words[2])
    {
        let pid: u32 = words[2].parse().ok()?;
        if !table.contains_key(&pid) {
            return Some((1, true, "b-pid", format!("kill -0 {pid}: pid gone")));
        }
        let (st, born) = (facts.start(pid)?, facts.start(shell)?);
        let status = if facts.uid(pid) == facts.my_uid() {
            0
        } else {
            1
        };
        if st > born {
            return Some((
                status,
                true,
                "b-pid",
                format!("kill -0 {pid}: pid reused by a process newer than the loop"),
            ));
        }
        return Some((status, false, "b-pid", format!("waits on live pid {pid}")));
    }

    enum Filter {
        Grep(GrepSpec),
        Take(String, usize),
    }
    let (files, test, neg, filters): (Vec<String>, Option<String>, bool, Vec<Filter>);
    if matches!(base.as_str(), "[" | "test" | "[[") && pipe.len() == 1 {
        let mut args: &[String] = words.get(1..)?;
        if base != "test" {
            let close = if base == "[" { "]" } else { "]]" };
            if args.last().map(String::as_str) != Some(close) {
                return None;
            }
            args = args.get(..args.len() - 1)?;
        }
        neg = args.first().map(String::as_str) == Some("!");
        if neg {
            args = args.get(1..)?;
        }
        if args.len() != 2 || !matches!(args[0].as_str(), "-f" | "-e" | "-s") {
            return None;
        }
        files = vec![args[1].clone()];
        test = Some(args[0].clone());
        filters = Vec::new();
    } else if base == "ps" || base == "pgrep" {
        return probe_atom(table, shell, pipe, facts);
    } else if matches!(base.as_str(), "cat" | "tail" | "head") || PROBES.contains(&base.as_str()) {
        let mut file_list: Vec<String> = inputs.clone();
        let mut flts = Vec::new();
        if PROBES.contains(&base.as_str()) {
            let spec = grep_spec(words)?;
            file_list.extend(spec.files.iter().cloned());
            flts.push(Filter::Grep(spec));
        } else {
            let rest = words.get(1..)?;
            let (mut n, mut k) = (None, 0usize);
            while let Some(a) = rest.get(k) {
                if base != "cat" && TAIL_COUNT.is_match(a) {
                    n = Some(a.trim_start_matches(['-', 'n']).parse::<usize>().ok()?);
                } else if base != "cat"
                    && a == "-n"
                    && rest.get(k + 1).is_some_and(|v| is_digits(v))
                {
                    n = Some(rest.get(k + 1)?.parse::<usize>().ok()?);
                    k += 1;
                } else if a.starts_with('-') {
                    return None; // tail -f, -c, ...: not a one-shot read
                } else {
                    file_list.push(a.clone());
                }
                k += 1;
            }
            let n = n.unwrap_or(10);
            if base == "tail" && n == 0 {
                // `tail -n0` prints nothing, but the Python models it as `lines[-0:]`,
                // every line. The two would disagree, so neither judges it.
                return None;
            }
            flts.push(Filter::Take(base.clone(), n));
        }
        for (words2, inputs2) in pipe.iter().skip(1) {
            if !PROBES.contains(&exe_base(words2.first()?).as_str()) {
                return None;
            }
            let spec = grep_spec(words2)?;
            if !spec.files.is_empty() || !inputs2.is_empty() {
                return None;
            }
            flts.push(Filter::Grep(spec));
        }
        if !flts.iter().any(|f| matches!(f, Filter::Grep(_))) {
            // No grep stage: the condition's status is the read's own exit status,
            // and `cat` of an empty file exits 0 however few lines it printed. Such
            // a loop may already have ended, with the shell in its post-loop work.
            return None;
        }
        files = file_list;
        test = None;
        neg = false;
        filters = flts;
    } else {
        return None;
    }
    if files.is_empty() {
        return None;
    }

    let (mut label, mut lines) = (Vec::new(), Vec::new());
    // Per file: `None` = gone; `Some(None)` = present but too large to read.
    let mut texts: Vec<Option<Option<String>>> = Vec::new();
    for f in &files {
        if !f.starts_with('/') {
            return None;
        }
        // One file that is not frozen settles the atom, and so the verdict: its
        // status no longer matters, and the costlier reads below are not needed.
        let unfrozen = |why: String| Some((1, false, "b-file", why));
        let harness = HARNESS_TASK_FILE_RE.is_match(f);
        match facts.stat(f)? {
            FileStat::Gone => {
                texts.push(None);
                // A missing file is usually one that does not exist YET: `until [ -f
                // build.done ]` waits on work that will create it, and no holder can
                // show for a file nobody has opened. The one exception is a harness
                // task's output file in its own session's tasks/ dir, still present:
                // the harness creates it when the task starts, so once it is gone
                // from a live tasks dir the task is gone and nothing will write it.
                let dir = f.rsplit_once('/').map_or("", |(d, _)| d);
                if harness && facts.isdir(dir) {
                    label.push(format!(
                        "{f} is gone from its session's tasks dir (its task ended)"
                    ));
                } else {
                    return unfrozen(format!("{f} does not exist yet"));
                }
            }
            FileStat::Present(mtime) => {
                let age = facts.now() - mtime;
                if age < (idle_min * 60) as f64 {
                    return unfrozen(format!("{f} changed {} ago", fmt_age(age)));
                }
                if let Some(holder) = facts.holders(f).iter().min() {
                    return unfrozen(format!("{f} is held open by pid {holder}"));
                }
                let needs_text = test.as_deref().is_none_or(|t| t == "-s");
                let text = if needs_text { facts.text(f)? } else { None };
                if test.is_none() {
                    let body = text.as_deref()?;
                    if body.contains('\r') {
                        // The Python reads text with universal newlines, so a `\r`
                        // splits a line there and not here: a line ending neither
                        // side models alike.
                        return None;
                    }
                    lines.extend(split_lines(body));
                }
                // A file that passes its own test, in a loop still looping, is a
                // wait for it to go away. Nothing holds a lock file open and its
                // mtime predicts nothing, so that is not proven frozen. A harness
                // task's output file is the exception: nothing else removes it.
                let passes = test.as_deref() != Some("-s") || text.as_deref() != Some("");
                if test.is_some() && passes && !harness {
                    return None;
                }
                label.push(format!(
                    "{f} unchanged for {}, no process holds it",
                    fmt_age(age)
                ));
                texts.push(Some(text));
            }
        }
    }
    let status = if let Some(test) = &test {
        let first = texts.first()?;
        let ok = match first {
            None => false,
            Some(text) => test != "-s" || text.as_deref() != Some(""),
        };
        i32::from(ok == neg)
    } else {
        for flt in &filters {
            match flt {
                Filter::Take(kind, n) => {
                    if kind == "tail" {
                        let from = lines.len().saturating_sub(*n);
                        lines.drain(..from);
                    } else if kind == "head" {
                        lines.truncate(*n);
                    }
                }
                Filter::Grep(spec) => {
                    let m = grep_matcher(spec)?;
                    lines.retain(|line| m(line));
                }
            }
        }
        i32::from(lines.is_empty())
    };
    Some((status, true, "b-file", label.join("; ")))
}

/// The line `ps` prints for `p`: the command alone for an args-only format, else
/// user, pid and ppid before it (`ps aux` / `ps -ef`). Other columns (%CPU, STAT,
/// TTY, times) are not modelled.
fn ps_line(table: &ProcTable, p: u32, facts: &dyn Facts, kind: PsKind) -> String {
    let cmd = joined_argv(table, p);
    if kind == PsKind::ArgsOnly {
        return cmd;
    }
    let ppid = table.get(&p).map_or(0, |(ppid, _)| *ppid);
    format!("{} {p} {ppid} {cmd}", facts.user_name(facts.uid(p)))
}

/// A `pgrep -f` or `ps | grep` condition, or `None`.
fn probe_atom(table: &ProcTable, shell: u32, pipe: &CondPipe, facts: &dyn Facts) -> Option<Atom> {
    let (words, inputs) = pipe.first()?;
    if !inputs.is_empty() {
        return None;
    }
    let is_pgrep = exe_base(words.first()?) == "pgrep";
    let hit: Box<dyn Fn(&str) -> bool>;
    let line: Box<dyn Fn(u32) -> String + '_>;
    let excluded: HashSet<u32>;
    let pats: HashSet<String>;
    if is_pgrep {
        if pipe.len() != 1 {
            return None;
        }
        let spec = pgrep_spec(words)?;
        let r = posix_re(&spec.pat, true)?;
        let r = if spec.exact {
            format!(r"\A(?:{r})\z")
        } else {
            r
        };
        let r = if spec.icase { format!("(?i){r}") } else { r };
        let rx = Regex::new(&r).ok()?;
        hit = Box::new(move |s| rx.is_match(s));
        line = Box::new(|p| joined_argv(table, p));
        // -A drops pgrep's ancestors: the loop shell and everything above it
        excluded = if spec.anc {
            let below: HashSet<u32> = subtree(table, shell)
                .into_iter()
                .filter(|&p| p != shell)
                .collect();
            own_tree(table, shell).difference(&below).copied().collect()
        } else {
            HashSet::new()
        };
        pats = HashSet::from([spec.pat]);
    } else {
        let kind = ps_all(words)?;
        if pipe.len() < 2 {
            return None;
        }
        let mut filters = Vec::new();
        let mut selected = HashSet::new();
        for (words2, inputs2) in pipe.iter().skip(1) {
            if !PROBES.contains(&exe_base(words2.first()?).as_str()) {
                return None;
            }
            let spec = grep_spec(words2)?;
            if !spec.files.is_empty() || !inputs2.is_empty() {
                return None;
            }
            filters.push(grep_matcher(&spec)?);
            if !spec.invert {
                selected.extend(spec.pats); // what the chain selects for, not what -v drops
            }
        }
        if selected.is_empty() {
            return None;
        }
        hit = Box::new(move |s| filters.iter().all(|m| m(s)));
        line = Box::new(move |p| ps_line(table, p, facts, kind));
        excluded = HashSet::new();
        pats = selected;
    }
    let self_match = !excluded.contains(&shell) && hit(&line(shell));
    if !self_match {
        // it ends when its target does: a real wait
        return Some((1, false, "a", "not self-matching".to_string()));
    }
    let target = table
        .iter()
        .filter(|(p, (_, argv))| !excluded.contains(p) && !argv.is_empty())
        .map(|(&p, _)| p)
        .filter(|&p| hit(&line(p)))
        .filter(|&p| {
            let argv = table.get(&p).map_or(&[][..], |(_, argv)| argv.as_slice());
            probes_of(argv, 0).is_disjoint(&pats)
        })
        .min();
    if let Some(target) = target {
        return Some((
            0,
            false,
            "a",
            format!("waits on live pid {target} (self-matching loop: will never exit on its own)"),
        ));
    }
    let shown = if is_pgrep { "pgrep" } else { "ps|grep" };
    let first = pats.iter().min().cloned().unwrap_or_default();
    Some((
        0,
        true,
        "a",
        format!("self-matching {shown} {first:?}, no live target"),
    ))
}

/// The verdict on one tool shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub stray: bool,
    /// `a`, `b-file`, `b-pid` or a `+` mix for a stray, `c` for a bounded wait,
    /// `None` otherwise.
    pub class: Option<String>,
    pub why: String,
}

fn not_stray(why: impl Into<String>) -> Verdict {
    Verdict {
        stray: false,
        class: None,
        why: why.into(),
    }
}

/// The loop a tool shell runs, once its command line has been read.
struct WaitLoop {
    kind: String,
    chain: Vec<(Option<&'static str>, bool, CondPipe)>,
}

/// The one unbounded until/while loop `argv` runs, or the verdict that it runs
/// none. Reads nothing but `argv`, so it is the cheap question to ask first.
fn wait_loop(argv: &[String]) -> Result<WaitLoop, Verdict> {
    let Some(cmd) = tool_command(argv) else {
        return Err(not_stray("command line not readable as a tool shell"));
    };
    if LOOP_WORD_RE.is_match(&cmd) && BOUNDED_RE.is_match(&cmd) {
        return Err(Verdict {
            stray: false,
            class: Some("c".to_string()),
            why: "bounded wait (a timeout, counter or deadline ends it)".to_string(),
        });
    }
    let Some(items) = parse_sh(&cmd) else {
        return Err(not_stray("not shell this classifier reads"));
    };
    let is_wait_loop = |(chain, sep): &(AndOr, Option<&'static str>)| {
        *sep != Some("&")
            && matches!(chain.as_slice(), [(_, (false, cmds))]
                if matches!(cmds.as_slice(), [Cmd::Loop { .. }]))
    };
    let loops: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| is_wait_loop(item))
        .map(|(k, _)| k)
        .collect();
    let [loop_at] = loops.as_slice() else {
        return Err(not_stray("not one wait loop"));
    };
    let mut env = Env::new();
    for (chain, sep) in items.iter().take(*loop_at) {
        if *sep == Some("&") {
            return Err(not_stray("starts a background job before its loop"));
        }
        let single = chain.len() == 1 && chain[0].1 .1.len() == 1;
        for (_, (_, cmds)) in chain {
            for c in cmds {
                if !pre_loop(c, &mut env, single) {
                    return Err(not_stray(
                        "runs more than assignments, cd and literal echo before its loop",
                    ));
                }
            }
        }
    }
    let Some(Cmd::Loop { kind, cond, body }) = items[*loop_at].0[0].1 .1.first() else {
        return Err(not_stray("not one wait loop"));
    };
    for (chain, _) in body {
        for (_, (_, cmds)) in chain {
            for c in cmds {
                if !body_only_waits(c) {
                    return Err(not_stray("the loop body does more than wait"));
                }
            }
        }
    }
    let Some(chain) = cond_atoms(cond, &env) else {
        return Err(not_stray("loop condition not readable"));
    };
    Ok(WaitLoop {
        kind: kind.clone(),
        chain,
    })
}

/// Stray only when the shell runs one unbounded until/while loop whose body only
/// sleeps, nothing but that loop's own probes and sleeps runs under it, every part
/// of its condition is frozen (class a: a self-matching pgrep -f / ps|grep with no
/// live target; b-file: a file no process holds open and unchanged past the idle
/// floor, or a harness task's output file that is gone; b-pid: a kill -0 pid that
/// is gone or was reused), and the condition still says "loop".
pub fn stray_verdict(table: &ProcTable, pid: u32, facts: &dyn Facts, idle_min: u64) -> Verdict {
    let Some((_, argv)) = table.get(&pid) else {
        return not_stray("not in the process table");
    };
    let WaitLoop { kind, chain } = match wait_loop(argv) {
        Ok(found) => found,
        Err(verdict) => return verdict,
    };
    let mut allowed: HashSet<String> = HashSet::from(["sleep".to_string()]);
    for (_, _, pipe) in &chain {
        for (words, _) in pipe {
            if let Some(w) = words.first() {
                allowed.insert(exe_base(w));
            }
        }
    }
    for d in subtree(table, pid) {
        if d == pid {
            continue;
        }
        let dv = table.get(&d).map_or(&[][..], |(_, a)| a.as_slice());
        if dv == argv.as_slice() {
            continue;
        }
        let harmless = dv.first().is_some_and(|head| {
            let base = exe_base(head);
            allowed.contains(&base)
                && !(base == "tail"
                    && dv.iter().skip(1).any(|a| {
                        a.starts_with("-f") || a.starts_with("-F") || a.starts_with("--follow")
                    }))
        });
        if !harmless {
            let shown: String = dv.join(" ").chars().take(60).collect();
            return not_stray(format!("runs {shown}"));
        }
    }
    let mut status: Option<i32> = None;
    let mut parts: Vec<Atom> = Vec::new();
    for (op, neg, pipe) in &chain {
        let Some(part) = atom(table, pid, pipe, facts, idle_min) else {
            return not_stray("loop condition not judged");
        };
        let s = if *neg { i32::from(part.0 == 0) } else { part.0 };
        let applies = match op {
            None => true,
            Some("&&") => status == Some(0),
            Some("||") => status != Some(0),
            Some(_) => false,
        };
        if applies {
            status = Some(s);
        }
        parts.push(part);
    }
    if let Some(live) = parts.iter().find(|a| !a.1) {
        return not_stray(live.3.clone());
    }
    if (status == Some(0)) != (kind == "while") {
        return not_stray("its condition already holds: the loop ends on its next pass");
    }
    let classes: BTreeSet<&str> = parts.iter().map(|a| a.2).collect();
    Verdict {
        stray: true,
        class: Some(classes.into_iter().collect::<Vec<_>>().join("+")),
        why: parts
            .iter()
            .map(|a| a.3.as_str())
            .collect::<Vec<_>>()
            .join("; "),
    }
}

fn body_only_waits(c: &Cmd) -> bool {
    let Cmd::Simple { words, redirs } = c else {
        return false;
    };
    let words: Option<Vec<String>> = words.iter().map(Word::lit).collect();
    let Some(words) = words else { return false };
    let Some(head) = words.first() else {
        return false;
    };
    LOOP_BODY.contains(&head.as_str())
        && !(head == "printf" && words.iter().any(|w| w == "-v"))
        && redirs.iter().all(|(op, target)| {
            let t = target.lit();
            t.as_deref() == Some("/dev/null")
                || (*op == ">&" && t.as_deref().is_some_and(is_digits))
        })
}

// ── the live /proc half ────────────────────────────────────────────────────────

/// A stray recorded before its pane closes: only these, still stray and unchanged,
/// are ever signalled afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedStray {
    pub pid: u32,
    /// Clock ticks since boot, so a reused pid cannot read as the same process.
    pub start: i64,
    pub argv: Vec<String>,
    pub class: String,
}

#[cfg(target_os = "linux")]
mod live {
    use super::wait_loop;
    use super::{exe_base, PROBES, SNAPSHOT};
    use super::{stray_verdict, subtree, Facts, FileStat, ProcTable, RecordedStray, FILE_MAX};
    use std::cell::OnceCell;
    use std::collections::{HashMap, HashSet};
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    /// `/proc/<pid>/stat`'s fields after the command name, so index 0 is field 3
    /// (state): ppid is `[1]`, pgid `[2]`, start time in clock ticks `[19]`.
    fn proc_stat(pid: u32) -> Option<Vec<String>> {
        // Bytes, not a string: `comm` is cut at 15 bytes, which can split a character,
        // and a process that drops out of the table here is a target nothing sees.
        let bytes = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
        let stat = String::from_utf8_lossy(&bytes);
        let (_, tail) = stat.rsplit_once(')')?;
        Some(tail.split_whitespace().map(str::to_string).collect())
    }

    fn proc_start(pid: u32) -> Option<i64> {
        proc_stat(pid)?.get(19)?.parse().ok()
    }

    fn read_argv(pid: u32) -> Option<Vec<String>> {
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        Some(
            String::from_utf8_lossy(&bytes)
                .split('\0')
                .filter(|a| !a.is_empty())
                .map(str::to_string)
                .collect(),
        )
    }

    pub(super) fn read_table() -> Option<ProcTable> {
        let mut table = ProcTable::new();
        for entry in std::fs::read_dir("/proc").ok()?.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
                continue;
            };
            let Some(ppid) = proc_stat(pid).and_then(|f| f.get(1)?.parse().ok()) else {
                continue;
            };
            let Some(argv) = read_argv(pid) else { continue };
            table.insert(pid, (ppid, argv));
        }
        Some(table)
    }

    pub(super) struct LiveFacts {
        now: f64,
        holders: OnceCell<HashMap<String, HashSet<u32>>>,
        users: OnceCell<HashMap<u32, String>>,
    }

    impl LiveFacts {
        pub(super) fn new() -> Self {
            Self {
                now: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64()),
                holders: OnceCell::new(),
                users: OnceCell::new(),
            }
        }

        /// Who has what open. A process whose fds this user cannot list (another
        /// uid's, or a non-dumpable one) is not seen.
        fn all_holders(&self) -> &HashMap<String, HashSet<u32>> {
            self.holders.get_or_init(|| {
                let mut out: HashMap<String, HashSet<u32>> = HashMap::new();
                let Ok(root) = std::fs::read_dir("/proc") else {
                    return out;
                };
                for entry in root.flatten() {
                    let Some(pid) = entry
                        .file_name()
                        .to_str()
                        .and_then(|s| s.parse::<u32>().ok())
                    else {
                        continue;
                    };
                    let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                        continue;
                    };
                    for fd in fds.flatten() {
                        if let Ok(target) = std::fs::read_link(fd.path()) {
                            let target = target.to_string_lossy();
                            let path = target.strip_suffix(" (deleted)").unwrap_or(&target);
                            out.entry(path.to_string()).or_default().insert(pid);
                        }
                    }
                }
                out
            })
        }
    }

    impl Facts for LiveFacts {
        fn start(&self, pid: u32) -> Option<i64> {
            proc_start(pid)
        }

        fn uid(&self, pid: u32) -> Option<u32> {
            std::fs::metadata(format!("/proc/{pid}"))
                .ok()
                .map(|m| m.uid())
        }

        fn my_uid(&self) -> Option<u32> {
            // SAFETY: getuid has no preconditions and cannot fail.
            Some(unsafe { libc::getuid() })
        }

        fn user_name(&self, uid: Option<u32>) -> String {
            let users = self.users.get_or_init(|| {
                let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
                passwd
                    .lines()
                    .filter_map(|line| {
                        let mut fields = line.split(':');
                        let name = fields.next()?;
                        let uid = fields.nth(1)?.parse().ok()?;
                        Some((uid, name.to_string()))
                    })
                    .collect()
            });
            match uid {
                Some(uid) => users.get(&uid).cloned().unwrap_or_else(|| uid.to_string()),
                None => "None".to_string(),
            }
        }

        fn isdir(&self, path: &str) -> bool {
            std::path::Path::new(path).is_dir()
        }

        fn stat(&self, path: &str) -> Option<FileStat> {
            let meta = match std::fs::metadata(path) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(FileStat::Gone),
                Err(_) => return None,
            };
            if !meta.file_type().is_file() {
                return None;
            }
            let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
            Some(FileStat::Present(mtime.as_secs_f64()))
        }

        fn holders(&self, path: &str) -> HashSet<u32> {
            let real = std::fs::canonicalize(path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string());
            self.all_holders().get(&real).cloned().unwrap_or_default()
        }

        fn text(&self, path: &str) -> Option<Option<String>> {
            if std::fs::metadata(path).ok()?.len() > FILE_MAX {
                return Some(None);
            }
            let bytes = std::fs::read(path).ok()?;
            Some(Some(String::from_utf8_lossy(&bytes).into_owned()))
        }

        fn now(&self) -> f64 {
            self.now
        }
    }

    fn is_tool_shell(argv: &[String]) -> bool {
        argv.iter().any(|a| a.contains(SNAPSHOT))
    }

    /// The tool shells in the subtree of `claudes` that are proven strays right now.
    pub(super) fn record(claudes: &[u32], idle_min: u64) -> Vec<RecordedStray> {
        let Some(table) = read_table() else {
            return Vec::new();
        };
        let facts = LiveFacts::new();
        let mut out = Vec::new();
        for &claude in claudes {
            for pid in subtree(&table, claude) {
                let Some((_, argv)) = table.get(&pid) else {
                    continue;
                };
                if !is_tool_shell(argv) {
                    continue;
                }
                let verdict = stray_verdict(&table, pid, &facts, idle_min);
                if let (true, Some(class), Some(start)) =
                    (verdict.stray, verdict.class, proc_start(pid))
                {
                    out.push(RecordedStray {
                        pid,
                        start,
                        argv: argv.clone(),
                        class,
                    });
                }
            }
        }
        out
    }

    /// Whether `pid` is still the recorded process: same start time and argv (a
    /// reused pid or a zombie is not).
    fn stray_now(stray: &RecordedStray) -> bool {
        read_argv(stray.pid).as_ref() == Some(&stray.argv)
            && proc_start(stray.pid) == Some(stray.start)
    }

    /// Why `stray` may NOT be signalled, if so: it must be
    /// the recorded process AND, on a fresh process table, still a stray of the same
    /// class. A loop whose condition flipped moves on to its post-loop commands in
    /// the same process, with the same argv and start time, so identity alone would
    /// not tell that work from the loop.
    fn still_stray(stray: &RecordedStray, idle_min: u64) -> Result<(), String> {
        if !stray_now(stray) {
            return Err("gone".to_string());
        }
        let Some(table) = read_table().filter(|t| t.contains_key(&stray.pid)) else {
            return Err("process table unreadable at re-check".to_string());
        };
        let verdict = stray_verdict(&table, stray.pid, &LiveFacts::new(), idle_min);
        if !verdict.stray || verdict.class.as_deref() != Some(stray.class.as_str()) {
            let why: String = verdict.why.chars().take(80).collect();
            return Err(format!("no longer a {} stray ({why})", stray.class));
        }
        Ok(())
    }

    /// Whether every member of the stray's process group is the stray itself, a fork
    /// of it, or one of its own sleeps and probes, so that signalling the group ends
    /// nothing but the loop.
    fn group_is_the_loop(table: &ProcTable, stray: &RecordedStray) -> bool {
        let mine: HashSet<u32> = subtree(table, stray.pid).into_iter().collect();
        let group = stray.pid.to_string();
        table.iter().all(|(&p, (_, argv))| {
            let in_group = proc_stat(p).is_some_and(|f| f.get(2) == Some(&group));
            if !in_group {
                return true;
            }
            mine.contains(&p)
                && (p == stray.pid
                    || *argv == stray.argv
                    || argv.first().is_some_and(|head| {
                        let base = exe_base(head);
                        matches!(base.as_str(), "sleep" | "ps" | "cat" | "tail" | "head")
                            || PROBES.contains(&base.as_str())
                    }))
        })
    }

    /// Send `sig` to the stray only while it is still the recorded process: through
    /// a pidfd where the kernel has one, so a pid reused after the check cannot be hit.
    fn signal_pid(stray: &RecordedStray, sig: libc::c_int) -> bool {
        let Ok(pid) = libc::pid_t::try_from(stray.pid) else {
            return false;
        };
        // SAFETY: pidfd_open takes two integers and touches no caller memory.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOSYS) {
                return false;
            }
            if !stray_now(stray) {
                return false;
            }
            // SAFETY: kill takes two integers and touches no caller memory.
            return unsafe { libc::kill(pid, sig) } == 0;
        }
        let sent = stray_now(stray)
            // SAFETY: a pidfd this function owns, a signal number, a null siginfo
            // (which the syscall documents as "as if by kill") and zero flags.
            && unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd,
                    sig,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            } == 0;
        // SAFETY: closes the fd opened above, exactly once.
        unsafe { libc::close(fd as libc::c_int) };
        sent
    }

    /// One log line per stray that outlived its pane. Each is ended only while it
    /// is still the recorded process AND still a stray of its recorded class on a
    /// fresh process table, checked again before every signal: SIGTERM, then SIGKILL
    /// after `wait`. The signal goes to its process group only when that group holds
    /// nothing but the loop and its own sleeps and probes, else to the pid alone.
    pub(super) fn end(recorded: &[RecordedStray], idle_min: u64, wait: Duration) -> Vec<String> {
        let mut lines = Vec::new();
        let mut gone = 0usize;
        for stray in recorded {
            if !stray_now(stray) {
                gone += 1;
                continue;
            }
            let (mut why, mut sent) = ("still alive after SIGKILL".to_string(), false);
            for (sig, grace) in [
                (libc::SIGTERM, wait),
                (libc::SIGKILL, Duration::from_secs(2)),
            ] {
                // The walk first: it is the slow part, and whatever it took, the
                // re-check and the signal below then sit next to each other.
                let group_ok = read_table().is_some_and(|t| group_is_the_loop(&t, stray));
                if let Err(refusal) = still_stray(stray, idle_min) {
                    if refusal != "gone" {
                        why = refusal;
                    }
                    break;
                }
                let Ok(pid) = libc::pid_t::try_from(stray.pid) else {
                    break;
                };
                // SAFETY: getpgid takes an integer and touches no caller memory.
                let leads_group = unsafe { libc::getpgid(pid) } == pid;
                if group_ok && leads_group {
                    if !stray_now(stray) {
                        break;
                    }
                    // A group id is not reused while its group exists.
                    // SAFETY: killpg takes two integers and touches no caller memory.
                    if unsafe { libc::killpg(pid, sig) } != 0 {
                        why = std::io::Error::last_os_error().to_string();
                        break;
                    }
                } else if !signal_pid(stray, sig) {
                    if stray_now(stray) {
                        why = std::io::Error::last_os_error().to_string();
                    }
                    break;
                }
                sent = true;
                let deadline = Instant::now() + grace;
                while Instant::now() < deadline && stray_now(stray) {
                    std::thread::sleep(Duration::from_millis(100));
                }
                if !stray_now(stray) {
                    break;
                }
            }
            if !stray_now(stray) && !sent {
                gone += 1; // it went by itself before anything was sent
                continue;
            }
            lines.push(if stray_now(stray) {
                format!(
                    "stray pid {} outlived the pane close; could not end: {why}",
                    stray.pid
                )
            } else {
                format!("stray pid {} outlived the pane close; ended", stray.pid)
            });
        }
        if gone > 0 {
            lines.insert(0, format!("{gone} stray wait loop(s) gone with the pane"));
        }
        lines
    }

    pub(super) fn verdicts(candidates: &[(u32, u32)], idle_min: u64) -> HashSet<u32> {
        // A finished session with a real child is ordinary and pays for its own
        // argv alone. Only a shell that really runs a wait loop costs a table walk.
        let loops: Vec<(u32, u32)> = candidates
            .iter()
            .copied()
            .filter(|&(_, pid)| read_argv(pid).is_some_and(|argv| wait_loop(&argv).is_ok()))
            .collect();
        if loops.is_empty() {
            return HashSet::new();
        }
        let Some(table) = read_table() else {
            return HashSet::new();
        };
        let facts = LiveFacts::new();
        loops
            .into_iter()
            // The snapshot named this pid a child of `parent`; the fresh table must
            // agree, or the pid was reused since and the verdict is about someone else.
            .filter(|&(parent, pid)| table.get(&pid).is_some_and(|(ppid, _)| *ppid == parent))
            .map(|(_, pid)| pid)
            .filter(|&pid| {
                let verdict = stray_verdict(&table, pid, &facts, idle_min);
                if verdict.stray {
                    tracing::debug!(
                        pid,
                        class = verdict.class.as_deref().unwrap_or(""),
                        why = %verdict.why,
                        "stray_wait: a finished session's shell is a stray wait loop, not work"
                    );
                }
                verdict.stray
            })
            .collect()
    }
}

/// Why a [`bounded`] read gave no answer. Every caller reads either as "no proven
/// strays"; only the census also cares which, because a read that is merely BUSY
/// says nothing about how slow the box is.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
enum Miss {
    /// Another read on the same flag is still running.
    Busy,
    /// It ran past its budget or panicked.
    Late,
}

/// Run `read` on the blocking pool, single-flight on `flag`, for at most `budget`.
/// A read still running past its budget keeps its flag.
#[cfg(target_os = "linux")]
async fn bounded_outcome<T: Send + 'static>(
    flag: &'static AtomicBool,
    budget: Duration,
    read: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Miss> {
    struct Release(&'static AtomicBool);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    if flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(Miss::Busy);
    }
    let release = Release(flag);
    let handle = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        let _release = release;
        read()
    });
    match tokio::time::timeout(budget, handle).await {
        Ok(Ok(value)) => Ok(value),
        _ => Err(Miss::Late),
    }
}

/// [`bounded_outcome`] for a caller that does not care why there was no answer.
#[cfg(target_os = "linux")]
async fn bounded<T: Send + 'static>(
    flag: &'static AtomicBool,
    budget: Duration,
    read: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    bounded_outcome(flag, budget, read).await.ok()
}

/// Which of `candidates` are proven-stray wait loops on a fresh process table.
/// Empty off Linux, past the budget, and whenever the table cannot be read: a shell
/// not proven stray is work.
pub async fn stray_shells(candidates: Vec<(u32, u32)>) -> HashSet<u32> {
    #[cfg(target_os = "linux")]
    {
        let now = unix_now_secs();
        if now < CENSUS_SKIP_UNTIL.load(Ordering::Acquire) {
            return HashSet::new();
        }
        match bounded_outcome(&CENSUS_IN_FLIGHT, CENSUS_BUDGET, move || {
            live::verdicts(&candidates, IDLE_MIN)
        })
        .await
        {
            Ok(strays) => strays,
            // A poll overlapping a healthy census is not evidence of a slow box.
            Err(Miss::Busy) => HashSet::new(),
            Err(Miss::Late) => {
                CENSUS_SKIP_UNTIL.store(census_skip_until(now), Ordering::Release);
                HashSet::new()
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (
            candidates,
            &CENSUS_IN_FLIGHT,
            CENSUS_BUDGET,
            &CENSUS_SKIP_UNTIL,
        );
        let _ = census_skip_until;
        let _ = unix_now_secs;
        HashSet::new()
    }
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// PURE: when a census that did not answer at `now` may next be attempted.
fn census_skip_until(now: u64) -> u64 {
    now.saturating_add(CENSUS_COOLDOWN_SECS)
}

/// Record the proven strays under `claudes` before their pane is closed. The
/// runner's pane kill reaches a stray background shell by neither route (the
/// in-process pane signals the root pid alone, the PTY holder signals the PTY child
/// and its group, and a background shell leads its own session and group), so what
/// is not recorded here would outlive the close as an invisible orphan.
pub async fn record_strays(claudes: Vec<u32>) -> Vec<RecordedStray> {
    if claudes.is_empty() {
        return Vec::new();
    }
    #[cfg(target_os = "linux")]
    {
        let recorded = bounded(&RECORD_IN_FLIGHT, RECORD_BUDGET, move || {
            live::record(&claudes, IDLE_MIN)
        })
        .await;
        if recorded.is_none() {
            tracing::warn!(
                "stray_wait: could not read this session's strays in time; any it holds will outlive its pane"
            );
        }
        recorded.unwrap_or_default()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (&RECORD_IN_FLIGHT, RECORD_BUDGET);
        Vec::new()
    }
}

/// End each recorded stray that outlived its pane's close; one line per stray to
/// log. The only place the runner signals a stray: never outside a pane close.
pub async fn end_strays(recorded: Vec<RecordedStray>) -> Vec<String> {
    if recorded.is_empty() {
        return Vec::new();
    }
    #[cfg(target_os = "linux")]
    {
        let count = u32::try_from(recorded.len()).unwrap_or(u32::MAX);
        let budget = END_BUDGET_PER_STRAY
            .saturating_mul(count)
            .min(END_BUDGET_MAX);
        bounded(&END_IN_FLIGHT, budget, move || {
            live::end(&recorded, IDLE_MIN, Duration::from_secs(5))
        })
        .await
        .unwrap_or_else(|| vec!["could not finish ending its stray wait loops in time".to_string()])
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (END_BUDGET_PER_STRAY, END_BUDGET_MAX, &END_IN_FLIGHT);
        Vec::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const NOW: f64 = 1e9;
    const ME: &str = "fixtureuser";
    const GP: &str = "git push -u origin HEAD";
    const CG: &str = "cargo-guard.sh test -- mcp::bridges";
    const OUT: &str =
        "/scratch/claude-1000/-s-proj/f116bbce-1e85-4828-a73f-06f0b90430b7/tasks/b1r1ys0pd.output";
    /// A harness tasks dir, present.
    const TDIR: &str = "/scratch/claude-1000/-s-proj/5b330eb3-5df7-459f-adf6-7927a533d510/tasks";

    /// A Linux tool shell's argv as Claude Code builds it (measured 2026-10-23).
    fn tool_argv(cmd: &str) -> Vec<String> {
        tool_argv_at(cmd, "/tmp/claude-1a2b-cwd")
    }

    fn tool_argv_at(cmd: &str, cwd_file: &str) -> Vec<String> {
        vec![
            "/bin/bash".to_string(),
            "-c".to_string(),
            format!(
                "source /x/.claude-sales/shell-snapshots/snapshot-bash-1-x.sh 2>/dev/null || true \
                 && shopt -u extglob 2>/dev/null || true && {{ \\builtin unalias -- 'unsetenv'; \
                 \\builtin unset -f -- 'unsetenv'; }} >/dev/null 2>&1 || true && eval '{}' \
                 < /dev/null && pwd -P >| {}",
                cmd.replace('\'', "'\"'\"'"),
                cwd_file
            ),
        ]
    }

    fn sv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    #[derive(Default)]
    struct Fake {
        start: HashMap<u32, i64>,
        files: HashMap<String, (f64, HashSet<u32>, Option<String>)>,
        dirs: HashSet<String>,
    }

    impl Facts for Fake {
        fn start(&self, pid: u32) -> Option<i64> {
            self.start.get(&pid).copied()
        }
        fn uid(&self, _pid: u32) -> Option<u32> {
            Some(1000)
        }
        fn my_uid(&self) -> Option<u32> {
            Some(1000)
        }
        fn user_name(&self, _uid: Option<u32>) -> String {
            ME.to_string()
        }
        fn isdir(&self, path: &str) -> bool {
            self.dirs.contains(path)
        }
        fn stat(&self, path: &str) -> Option<FileStat> {
            Some(match self.files.get(path) {
                None => FileStat::Gone,
                Some((mtime, ..)) => FileStat::Present(*mtime),
            })
        }
        fn holders(&self, path: &str) -> HashSet<u32> {
            self.files
                .get(path)
                .map(|(_, holders, _)| holders.clone())
                .unwrap_or_default()
        }
        fn text(&self, path: &str) -> Option<Option<String>> {
            self.files.get(path).map(|(.., text)| text.clone())
        }
        fn now(&self) -> f64 {
            NOW
        }
    }

    type FileRow = (&'static str, f64, &'static [u32], Option<&'static str>);

    fn old(path: &'static str) -> FileRow {
        (path, NOW - 3.0 * 3600.0, &[], Some("Compiling x\n"))
    }

    fn held(path: &'static str) -> FileRow {
        (path, NOW - 3.0 * 3600.0, &[950], Some("Compiling x\n"))
    }

    /// The fixture `one()`: the shell under test is pid 40, under claude 10.
    fn one_with_dirs(
        cmd: &str,
        extra: &[(u32, u32, &[&str])],
        files: &[FileRow],
        start: &[(u32, i64)],
        kids: &[&[&str]],
        dirs: Option<&[&str]>,
    ) -> Verdict {
        let mut t: ProcTable = HashMap::new();
        t.insert(1, (0, sv(&["/sbin/init"])));
        t.insert(10, (1, sv(&["/usr/bin/claude"])));
        t.insert(40, (10, tool_argv(cmd)));
        for (k, argv) in kids.iter().enumerate() {
            t.insert(41 + k as u32, (40, sv(argv)));
        }
        for (pid, ppid, argv) in extra {
            t.insert(*pid, (*ppid, sv(argv)));
        }
        let mut fake = Fake::default();
        for (path, mtime, holders, text) in files {
            fake.files.insert(
                (*path).to_string(),
                (
                    *mtime,
                    holders.iter().copied().collect(),
                    text.map(str::to_string),
                ),
            );
        }
        fake.start = start.iter().copied().collect();
        fake.dirs = match dirs {
            Some(d) => d.iter().map(|s| (*s).to_string()).collect(),
            None => {
                let mut d: HashSet<String> = [
                    TDIR,
                    "/s/proj/tasks",
                    "/s/ci/tasks",
                    "/s/jobs/0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b/tasks",
                ]
                .iter()
                .map(|s| (*s).to_string())
                .collect();
                d.extend(
                    files
                        .iter()
                        .filter_map(|(p, ..)| p.rsplit_once('/').map(|(dir, _)| dir.to_string())),
                );
                d
            }
        };
        stray_verdict(&t, 40, &fake, 15)
    }

    fn one(
        cmd: &str,
        extra: &[(u32, u32, &[&str])],
        files: &[FileRow],
        start: &[(u32, i64)],
        kids: &[&[&str]],
    ) -> Verdict {
        one_with_dirs(cmd, extra, files, start, kids, None)
    }

    const SLEEP: &[&[&str]] = &[&["sleep", "15"]];

    /// The measured `d2dd1423` table: 8 self-matching loops on 2 patterns, each with a
    /// sleep child, under claude 21033.
    fn d2() -> (ProcTable, Vec<u32>) {
        let loops: Vec<(u32, String)> = vec![
            (445646, format!("until ! pgrep -f \"{GP}\" >/dev/null; do sleep 15; done; cat {OUT}")),
            (464345, format!("until ! pgrep -f \"{GP}\" >/dev/null; do sleep 15; done; cat {OUT}")),
            (1873808, format!("until ! pgrep -f \"{GP}\" >/dev/null; do sleep 15; done; cat {OUT}")),
            (3135797, format!("until ! pgrep -f \"{GP}\" >/dev/null; do sleep 15; done; cat {OUT}")),
            (3139942, format!("until ! pgrep -f \"{GP}\" >/dev/null; do sleep 15; done; echo finished")),
            (
                2786305,
                format!(
                    "until ! pgrep -f \"{CG}\" >/dev/null; do sleep 15; done; grep -E \"^error|bridges::|test result: ok. [1-9]|FAILED|verdict\" /s/cargotest.log | head"
                ),
            ),
            (3690915, format!("until ! pgrep -f \"{CG}\" >/dev/null; do sleep 15; done; echo done")),
            (
                3743706,
                format!("until ! pgrep -f \"{CG}\" >/dev/null; do sleep 15; done; cat {TDIR}/b2rb7udxa.output"),
            ),
        ];
        let mut t: ProcTable = HashMap::new();
        t.insert(1, (0, sv(&["/sbin/init"])));
        t.insert(20, (1, sv(&["pty-holder", "--", "claude"])));
        t.insert(
            21033,
            (20, sv(&["/usr/bin/claude", "--resume", "f116bbce"])),
        );
        t.insert(50, (1, sv(&["vim", "notes.txt"])));
        let mut pids = Vec::new();
        for (pid, cmd) in loops {
            t.insert(pid, (21033, tool_argv(&cmd)));
            t.insert(pid + 1, (pid, sv(&["sleep", "15"])));
            pids.push(pid);
        }
        // A loop's own probe caught mid-flight is a watcher, not a target.
        t.insert(445648, (445646, sv(&["pgrep", "-f", GP])));
        (t, pids)
    }

    fn fake_d2() -> Fake {
        Fake::default()
    }

    #[test]
    fn the_measured_d2dd1423_set_is_all_stray() {
        let (t, pids) = d2();
        for pid in pids {
            let v = stray_verdict(&t, pid, &fake_d2(), 15);
            assert!(v.stray, "{pid}: {v:?}");
            assert_eq!(v.class.as_deref(), Some("a"), "{pid}");
        }
    }

    #[test]
    fn a_real_cargo_test_beside_the_strays_is_work() {
        let (mut t, pids) = d2();
        t.insert(
            700,
            (
                21033,
                tool_argv("cargo test -p qontinui-runner 2>&1 | tail -20"),
            ),
        );
        t.insert(701, (700, sv(&["cargo", "test", "-p", "qontinui-runner"])));
        assert!(!stray_verdict(&t, 700, &fake_d2(), 15).stray);
        for pid in pids {
            assert!(stray_verdict(&t, pid, &fake_d2(), 15).stray, "{pid}");
        }
    }

    #[test]
    fn a_self_matching_loop_whose_target_still_runs_waits_on_live_work() {
        let (mut t, pids) = d2();
        t.insert(800, (1, sv(&["git", "push", "-u", "origin", "HEAD"])));
        let (mut work, mut strays) = (0, 0);
        for pid in pids {
            let v = stray_verdict(&t, pid, &fake_d2(), 15);
            if v.stray {
                strays += 1;
            } else {
                work += 1;
                assert_eq!(
                    v.why,
                    "waits on live pid 800 (self-matching loop: will never exit on its own)"
                );
            }
        }
        assert_eq!((work, strays), (5, 3));
    }

    /// The runner's own subtree holds the sessions, so the Python's own-tree
    /// exclusion must not be carried over: a target under the same runner is a
    /// live target, not "the screener".
    #[test]
    fn a_target_in_the_same_process_tree_is_a_live_target() {
        let (mut t, pids) = d2();
        t.insert(800, (21033, sv(&["git", "push", "-u", "origin", "HEAD"])));
        assert!(!stray_verdict(&t, pids[0], &fake_d2(), 15).stray);
    }

    #[test]
    fn a_shell_whose_command_line_is_not_readable_is_work() {
        let mut t: ProcTable = HashMap::new();
        t.insert(7, (1, sv(&["bash.exe"])));
        assert!(!stray_verdict(&t, 7, &Fake::default(), 15).stray);
        assert!(!one("x", &[], &[], &[], SLEEP).stray);
    }

    #[test]
    fn the_case_table() {
        let task: &'static str = Box::leak(format!("{TDIR}/brm8jg1o0.output").into_boxed_str());
        let twait =
            format!("until grep -qE 'exit=' {task} 2>/dev/null; do sleep 20; done; cat {task}");
        let census = "/s/census/cargo.log";
        let esc = format!(
            "L={census}; until grep -qE 'test result|^error|could not compile' $L || ! pgrep -f 'cargo test -- zz_census' >/dev/null; do sleep 15; done; tail -8 $L"
        );
        let s20: &[&[&str]] = &[&["sleep", "20"]];
        let none: &[&[&str]] = &[];
        type Row<'a> = (
            bool,
            Option<&'a str>,
            String,
            Vec<(u32, u32, &'a [&'a str])>,
            Vec<FileRow>,
            Vec<(u32, i64)>,
            &'a [&'a [&'a str]],
        );
        let rows: Vec<Row> = vec![
            // (5) a tasks/<id>.output marker wait
            (true, Some("b-file"), twait.clone(), vec![], vec![old(task)], vec![], s20),
            (false, None, twait.clone(), vec![], vec![held(task)], vec![], s20),
            // changed inside the idle floor
            (false, None, twait.clone(), vec![], vec![(task, NOW - 60.0, &[], Some("x\n"))], vec![], none),
            // already holds
            (false, None, twait.clone(), vec![], vec![(task, NOW - 3.0 * 3600.0, &[], Some("exit=0\n"))], vec![], none),
            // a harness task file gone from a live tasks dir
            (true, Some("b-file"), twait.clone(), vec![], vec![], vec![], none),
            // a tasks/*.output outside the harness layout is an ordinary file: missing means "not yet"
            (false, None, "until [ -f /s/proj/tasks/build.output ]; do sleep 5; done; ./deploy.sh".into(), vec![], vec![], vec![], none),
            (false, None, "until grep -q DONE /s/ci/tasks/job42.output; do sleep 30; done; ./release.sh".into(), vec![], vec![], vec![], none),
            // a uuid dir outside the harness scratch layout
            (false, None, "until [ -f /s/jobs/0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b/tasks/out.output ]; do sleep 5; done; ./ship.sh".into(), vec![], vec![], vec![], none),
            // a prefix assignment's persistence depends on the shell
            (false, None, "L=/s/a.log; L=/s/b.log :; until grep -q DONE $L; do sleep 5; done".into(), vec![], vec![old("/s/a.log"), old("/s/b.log")], vec![], none),
            // only inert commands may precede the loop: a detached producer there may be the writer
            (false, None, "setsid -f bash -c 'sleep 3600; echo DONE >> /s/x.log'; until grep -q DONE /s/x.log; do sleep 5; done".into(), vec![], vec![old("/s/x.log")], vec![], none),
            // a value can hide a command substitution
            (false, None, "X=$(setsid -f prod); until grep -q DONE /s/x.log; do sleep 5; done".into(), vec![], vec![old("/s/x.log")], vec![], none),
            (true, Some("b-file"), "S=/s; cd $S; export L=$S/x.log; echo watching; until grep -q DONE $L; do sleep 5; done".into(), vec![], vec![old("/s/x.log")], vec![], none),
            // a missing file that is not a task file does not exist YET: the standard wait on real work
            (false, None, "until [ -f /s/build.done ]; do sleep 10; done; ./deploy.sh".into(), vec![], vec![], vec![], none),
            (false, None, "while [ ! -f /s/build.done ]; do sleep 10; done; git push".into(), vec![], vec![], vec![], none),
            (false, None, "until grep -q DONE /s/build.log 2>/dev/null; do sleep 10; done; echo ok".into(), vec![], vec![], vec![], none),
            // brace expansion and a changed IFS make the path unknown, even when a file of that literal name is frozen
            (false, None, "until grep -q DONE /s/{build,test}.log; do sleep 5; done".into(), vec![], vec![old("/s/{build,test}.log")], vec![], none),
            (false, None, "IFS=,; L=/s/a.log,/s/b; until grep -q DONE $L; do sleep 5; done".into(), vec![], vec![old("/s/a.log,/s/b")], vec![], none),
            // a body that does more than wait: a command substitution, printf -v, a write
            (false, None, format!("until ! pgrep -f \"{GP}\"; do sleep $(git push -u origin HEAD; echo 5); done"), vec![], vec![], vec![], none),
            (false, None, "until ! pgrep -f \"zzq\"; do echo $(make deploy); sleep 5; done".into(), vec![], vec![], vec![], none),
            (false, None, "L=/s/a; until grep -q DONE $L; do printf -v L %s /s/b; sleep 5; done".into(), vec![], vec![old("/s/a")], vec![], none),
            (false, None, "until grep -q DONE /s/a.log; do echo tick >> /s/a.log; sleep 5; done".into(), vec![], vec![old("/s/a.log")], vec![], none),
            // redirects to /dev/null are still only waiting
            (true, Some("b-file"), "until grep -q DONE /s/a.log; do sleep 5 >/dev/null 2>&1; done".into(), vec![], vec![old("/s/a.log")], vec![], none),
            // a job backgrounded before the loop may still be the writer
            (false, None, format!("(sleep 9; echo x > {task}) & until [ -s {task} ]; do sleep 5; done"), vec![], vec![(task, NOW - 7200.0, &[], Some(""))], vec![], none),
            (true, Some("b-file"), format!("until [ -s {task} ]; do sleep 5; done"), vec![], vec![(task, NOW - 7200.0, &[], Some(""))], vec![], none),
            (false, None, format!("until [ -s {task} ]; do sleep 5; done"), vec![], vec![(task, NOW - 7200.0, &[], Some("x"))], vec![], none),
            // the census's `|| ! pgrep -f` escape, with $L expanded from its literal assignment
            (true, Some("a+b-file"), esc.clone(), vec![], vec![old(census)], vec![], none),
            (false, None, esc.clone(), vec![(90, 1, &["cargo", "test", "--", "zz_census"][..])], vec![old(census)], vec![], none),
            // unexpandable
            (false, None, "until grep -q x $NOPE; do sleep 5; done".into(), vec![], vec![], vec![], none),
            // an unquoted value that word-splits is not one known path
            (false, None, format!("L='{task} x'; until grep -q DONE $L; do sleep 5; done"), vec![], vec![old(task)], vec![], none),
            // its own sleep and probe only
            (true, Some("b-file"), format!("until grep -q DONE {task}; do sleep 5; done"), vec![], vec![old(task)], vec![], &[&["sleep", "5"], &["grep", "-q", "DONE", "x"]]),
            // work running under the loop
            (false, None, format!("until grep -q DONE {task}; do sleep 5; done"), vec![], vec![old(task)], vec![], &[&["sleep", "5"], &["cargo", "build"]]),
            // the body works
            (false, None, format!("until ! pgrep -f \"{GP}\"; do cargo build; sleep 5; done"), vec![], vec![], vec![], none),
            // (7) bounded waits: class (c), never stray
            (false, Some("c"), format!("timeout 600 bash -c 'until ! pgrep -f \"{GP}\" >/dev/null; do sleep 5; done'"), vec![], vec![], vec![], &[&["timeout", "600", "bash", "-c", "..."]]),
            (false, Some("c"), format!("i=0; until ! pgrep -f \"{GP}\"; do sleep 5; i=$((i+1)); done"), vec![], vec![], vec![], none),
            (false, Some("c"), format!("end=$(( $(date +%s) + 600 )); while [ $(date +%s) -lt $end ] && pgrep -f \"{GP}\"; do sleep 5; done"), vec![], vec![], vec![], none),
            // self-excluding patterns are real waits, not class (a)
            (false, None, "until ! pgrep -f '[g]it push' >/dev/null; do sleep 15; done".into(), vec![], vec![], vec![], none),
            (false, None, format!("until ! pgrep -fx \"{GP}\" >/dev/null; do sleep 15; done"), vec![], vec![], vec![], none),
            (false, None, format!("until ! pgrep -fA \"{GP}\" >/dev/null; do sleep 15; done"), vec![], vec![], vec![], none),
            // no -f: names, not argv
            (false, None, "until ! pgrep cargo; do sleep 15; done".into(), vec![], vec![], vec![], none),
            (true, Some("a"), "while ps aux | grep cargo >/dev/null; do sleep 5; done".into(), vec![], vec![], vec![], none),
            // -v grep drops the loop's own line too, so it ends with its target
            (false, None, "while ps aux | grep cargo | grep -v grep >/dev/null; do sleep 5; done".into(), vec![], vec![], vec![], none),
            // ps -p: selective
            (false, None, "while ps -p 4242 | grep -q 4242; do sleep 5; done".into(), vec![], vec![], vec![], none),
            // `ps aux` prints the user column: a pattern on it still finds the live target
            (false, None, format!("while ps aux | grep -q \"{ME}.*cargo build\"; do sleep 5; done"), vec![(90, 1, &["cargo", "build"][..])], vec![], vec![], none),
            (true, Some("a"), format!("while ps aux | grep -q \"{ME}.*cargo build\"; do sleep 5; done"), vec![], vec![], vec![], none),
            (false, None, "while ps -eo args | grep -q \"^cargo build\"; do sleep 5; done".into(), vec![], vec![], vec![], none),
            // kill -0: reused pid -> stray; the same process -> a real wait; a gone pid ends a while-loop
            (true, Some("b-pid"), "while kill -0 4242 2>/dev/null; do sleep 5; done".into(), vec![(4242, 1, &["daemon"][..])], vec![], vec![(40, 100), (4242, 200)], none),
            (false, None, "while kill -0 4242 2>/dev/null; do sleep 5; done".into(), vec![(4242, 1, &["make"][..])], vec![], vec![(40, 100), (4242, 50)], none),
            (false, None, "while kill -0 4243 2>/dev/null; do sleep 5; done".into(), vec![], vec![], vec![(40, 100)], none),
            // backgrounded
            (false, None, format!("until ! pgrep -f \"{GP}\"; do sleep 5; done & wait"), vec![], vec![], vec![], none),
            // heredoc: not read
            (false, None, "cat <<EOF\nx\nEOF".into(), vec![], vec![], vec![], none),
            // a wait for a file to GO is not proven frozen: nothing holds a lock file open
            (false, None, "while [ -f /s/lock ]; do sleep 5; done; ./next.sh".into(), vec![], vec![old("/s/lock")], vec![], none),
            (false, None, "until [ ! -e /s/lock ]; do sleep 5; done; ./next.sh".into(), vec![], vec![old("/s/lock")], vec![], none),
            (false, None, "while [ -s /s/queue ]; do sleep 5; done".into(), vec![], vec![old("/s/queue")], vec![], none),
            // ... unless it is a harness task's own output file
            (true, Some("b-file"), format!("while [ -f {task} ]; do sleep 5; done"), vec![], vec![old(task)], vec![], none),
            // a tail, head or cat read in the condition
            (true, Some("b-file"), "until tail -n 2 /s/a.log | grep -q DONE; do sleep 5; done".into(), vec![], vec![("/s/a.log", NOW - 7200.0, &[], Some("DONE\nb\nc\n"))], vec![], none),
            (false, None, "until tail -1 /s/a.log | grep -q DONE; do sleep 5; done".into(), vec![], vec![("/s/a.log", NOW - 7200.0, &[], Some("b\nDONE\n"))], vec![], none),
            (true, Some("b-file"), "until head -1 /s/a.log | grep -q DONE; do sleep 5; done".into(), vec![], vec![("/s/a.log", NOW - 7200.0, &[], Some("b\nDONE\n"))], vec![], none),
            (false, None, "until cat /s/a.log | grep -q DONE; do sleep 5; done".into(), vec![], vec![("/s/a.log", NOW - 7200.0, &[], Some("b\nDONE\n"))], vec![], none),
            // tail -f is not a one-shot read
            (false, None, "until tail -f /s/a.log | grep -q DONE; do sleep 5; done".into(), vec![], vec![old("/s/a.log")], vec![], none),
        ];
        let mut bad = Vec::new();
        for (want, cls, cmd, extra, files, start, kids) in &rows {
            let got = one(cmd, extra, files, start, kids);
            if got.stray != *want || (cls.is_some() && got.class.as_deref() != *cls) {
                bad.push(format!(
                    "{cmd:?} -> {got:?}, want stray={want} class={cls:?}"
                ));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
        // The harness layout, but its tasks dir is gone too: the path is not known
        // to be the harness's.
        assert!(!one_with_dirs(&twait, &[], &[], &[], none, Some(&[])).stray);
    }

    #[test]
    fn tool_command_round_trips() {
        assert_eq!(
            tool_command(&tool_argv("echo 'a b' \"$X\"")).as_deref(),
            Some("echo 'a b' \"$X\"")
        );
    }

    #[test]
    fn posix_translation_cases() {
        for (pat, ere, line, want) in [
            (
                r"test rc=\|clippy rc=[1-9]",
                false,
                "clippy rc=2",
                Some(true),
            ),
            (r"error(\[|:)", true, "error[E0425]", Some(true)),
            ("a+", false, "aa", Some(false)),
            ("a+", false, "a+", Some(true)),
            ("[[:alpha:]]", true, "x", None),
            ("(?i)OK", true, "ok", None),
            ("^*x", false, "x", None),
        ] {
            let got = posix_re(pat, ere).map(|r| Regex::new(&r).unwrap().is_match(line));
            assert_eq!(got, want, "{pat}");
        }
    }

    /// `grep -w` is written without lookaround: the edges still have to be real.
    #[test]
    fn grep_word_and_line_options_keep_their_meaning() {
        let m = |args: &[&str], line: &str| {
            let spec = grep_spec(&sv(args)).unwrap();
            grep_matcher(&spec).unwrap()(line)
        };
        assert!(m(&["grep", "-w", "done"], "all done now"));
        assert!(m(&["grep", "-w", "done"], "done"));
        assert!(!m(&["grep", "-w", "done"], "undone"));
        assert!(!m(&["grep", "-w", "done"], "done_ok"));
        assert!(m(&["grep", "-x", "done"], "done"));
        assert!(!m(&["grep", "-x", "done"], "all done"));
        assert!(m(&["grep", "-vi", "DONE"], "running"));
        assert!(!m(&["grep", "-vi", "DONE"], "done"));
        assert!(m(&["grep", r"\<done\>"], "is done."));
        assert!(!m(&["grep", r"\<done\>"], "undone"));
    }

    /// The Python raises `RecursionError` and reads "not stray". Here it would be a
    /// stack overflow that takes the whole runner down, so the depth is bounded.
    #[test]
    fn a_deeply_nested_command_is_refused_not_a_stack_overflow() {
        let deep = format!("{}true{}", "(".repeat(200_000), ")".repeat(200_000));
        let handle = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || parse_sh(&deep).is_none())
            .unwrap();
        assert!(handle.join().unwrap());
        // Ordinary nesting is unaffected.
        assert!(parse_sh(&format!("{}true{}", "(".repeat(8), ")".repeat(8))).is_some());
    }

    #[cfg(target_os = "linux")]
    mod live_process {
        use super::*;
        use std::os::unix::process::CommandExt;
        use std::process::{Child, Command, Stdio};

        /// Ends the loop however the test ends: a failed assert must not leave it
        /// polling for ever, which is the very thing this module exists to find.
        struct Reaped(Child);

        impl Drop for Reaped {
            fn drop(&mut self) {
                // Its group, so what it runs after the loop goes with it.
                if let Ok(group) = i32::try_from(self.0.id()) {
                    // SAFETY: killpg takes two integers and touches no caller memory.
                    unsafe { libc::killpg(group, libc::SIGKILL) };
                }
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        /// Starts in `group`, or its own when 0, as a background shell does; and dies
        /// with this test binary however it dies.
        fn spawn(argv: &[String], group: i32) -> Child {
            let mut cmd = Command::new(&argv[0]);
            cmd.args(&argv[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(group);
            // SAFETY: prctl with these arguments is async-signal-safe and touches no
            // memory of the parent.
            unsafe {
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                    Ok(())
                });
            }
            cmd.spawn().unwrap()
        }

        /// Wait out the fork-to-exec window: until then a child's argv is still the
        /// test binary's. A deadline this long only matters on a box under real load.
        fn wait_for_argv(pid: u32, needle: &str) {
            for _ in 0..600 {
                let seen = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                if String::from_utf8_lossy(&seen).contains(needle) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            panic!("pid {pid} never ran {needle}");
        }

        fn start_ticks(pid: u32) -> i64 {
            let bytes = std::fs::read(format!("/proc/{pid}/stat")).unwrap();
            String::from_utf8_lossy(&bytes)
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .nth(19)
                .unwrap()
                .parse()
                .unwrap()
        }

        /// The tool-shell loop `cmd`. Polled files are frozen because the floor the
        /// tests pass is 0.
        fn start_loop(dir: &std::path::Path, cmd: &str, class: &str) -> (Reaped, RecordedStray) {
            let cwd_file = dir.join("cwd");
            let argv = tool_argv_at(cmd, cwd_file.to_str().unwrap());
            let child = spawn(&argv, 0);
            let pid = child.id();
            wait_for_argv(pid, "snapshot-bash");
            let recorded = RecordedStray {
                pid,
                start: start_ticks(pid),
                argv,
                class: class.to_string(),
            };
            (Reaped(child), recorded)
        }

        fn poll_file_loop(file: &std::path::Path) -> String {
            format!("until grep -q DONE {}; do sleep 1; done", file.display())
        }

        /// Reap it, so a zombie's still-present pid cannot read as alive.
        fn reaped_shortly(child: &mut Reaped) -> bool {
            for _ in 0..40 {
                if child.0.try_wait().unwrap().is_some() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        }

        #[test]
        fn a_recorded_unchanged_stray_is_ended_and_nothing_else_is() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("x.log");
            std::fs::write(&file, "Compiling\n").unwrap();
            let (mut child, stray) = start_loop(dir.path(), &poll_file_loop(&file), "b-file");

            // The classifier reads the real process as a stray ...
            let table = live::read_table().unwrap();
            let verdict = stray_verdict(&table, stray.pid, &live::LiveFacts::new(), 0);
            assert!(verdict.stray, "{verdict:?}");
            assert_eq!(verdict.class.as_deref(), Some("b-file"));

            // ... an entry whose start time or argv is wrong is not signalled ...
            let wrong_start = RecordedStray {
                start: stray.start + 1,
                ..stray.clone()
            };
            let mut wrong_argv = stray.clone();
            wrong_argv.argv.push("x".to_string());
            let lines = live::end(&[wrong_start, wrong_argv], 0, Duration::from_millis(500));
            assert_eq!(lines, vec!["2 stray wait loop(s) gone with the pane"]);
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "the loop was signalled"
            );

            // ... and the recorded one is.
            let lines = live::end(std::slice::from_ref(&stray), 0, Duration::from_secs(2));
            assert_eq!(
                lines,
                vec![format!(
                    "stray pid {} outlived the pane close; ended",
                    stray.pid
                )]
            );
            assert!(reaped_shortly(&mut child));
        }

        /// The class the plan exists for, on a real process: `pgrep -f` sees the
        /// loop's own command line, and the loop's own `pgrep` is not a target.
        #[test]
        fn a_real_self_matching_loop_is_stray_until_its_target_runs() {
            let dir = tempfile::tempdir().unwrap();
            let token = format!("stray-wait-selfmatch-{}", std::process::id());
            let cmd = format!("until ! pgrep -f \"{token}\" >/dev/null; do sleep 1; done");
            let (mut child, stray) = start_loop(dir.path(), &cmd, "a");
            let read = || {
                let table = live::read_table().unwrap();
                stray_verdict(&table, stray.pid, &live::LiveFacts::new(), 0)
            };
            let verdict = read();
            assert!(verdict.stray, "{verdict:?}");
            assert_eq!(verdict.class.as_deref(), Some("a"));

            // Its `$0`: a `sleep` given a stray argument would just exit.
            let target = spawn(&sv(&["bash", "-c", "sleep 3600; :", &token]), 0);
            let target = Reaped(target);
            wait_for_argv(target.0.id(), &token);
            let verdict = read();
            assert!(!verdict.stray, "{verdict:?}");
            assert!(
                verdict
                    .why
                    .starts_with(&format!("waits on live pid {}", target.0.id())),
                "{verdict:?}"
            );
            drop(target);

            let lines = live::end(std::slice::from_ref(&stray), 0, Duration::from_secs(2));
            assert_eq!(
                lines,
                vec![format!(
                    "stray pid {} outlived the pane close; ended",
                    stray.pid
                )]
            );
            assert!(reaped_shortly(&mut child));
        }

        /// The census judges a candidate only while the fresh table still names the
        /// parent the snapshot named: a pid reused since then is someone else's shell.
        #[test]
        fn a_candidate_whose_parent_changed_is_not_judged() {
            let dir = tempfile::tempdir().unwrap();
            let token = format!("stray-wait-parent-{}", std::process::id());
            let cmd = format!("until ! pgrep -f \"{token}\" >/dev/null; do sleep 1; done");
            let (_child, stray) = start_loop(dir.path(), &cmd, "a");
            let parent = live::read_table().unwrap()[&stray.pid].0;
            assert_eq!(
                live::verdicts(&[(parent, stray.pid)], 0),
                [stray.pid].into_iter().collect::<HashSet<u32>>()
            );
            let not_the_parent = parent.wrapping_add(1).max(1);
            assert!(live::verdicts(&[(not_the_parent, stray.pid)], 0).is_empty());
        }

        /// A loop whose condition flipped moves on to its post-loop commands in the
        /// same process, with the same argv and start time: identity alone cannot
        /// tell, so the re-check on a fresh table has to, and that work must survive.
        #[test]
        fn a_loop_whose_condition_flipped_is_not_signalled() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("x.log");
            std::fs::write(&file, "Compiling\n").unwrap();
            let cmd = format!("{}; sleep 3600", poll_file_loop(&file));
            let (mut child, stray) = start_loop(dir.path(), &cmd, "b-file");
            std::fs::write(&file, "DONE\n").unwrap();
            let lines = live::end(std::slice::from_ref(&stray), 0, Duration::from_millis(300));
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert!(
                lines[0].contains("could not end: no longer a b-file stray"),
                "{lines:?}"
            );
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "the post-loop work died"
            );
        }

        /// A process group holding anything but the loop is not signalled as a group.
        #[test]
        fn a_foreign_member_of_the_group_survives() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("x.log");
            std::fs::write(&file, "Compiling\n").unwrap();
            let (mut child, stray) = start_loop(dir.path(), &poll_file_loop(&file), "b-file");
            let group = i32::try_from(stray.pid).unwrap();
            let foreign = Reaped(spawn(&["sleep".to_string(), "3600".to_string()], group));
            let foreign_pid = foreign.0.id();
            wait_for_argv(foreign_pid, "3600");
            let lines = live::end(std::slice::from_ref(&stray), 0, Duration::from_secs(2));
            assert_eq!(
                lines,
                vec![format!(
                    "stray pid {} outlived the pane close; ended",
                    stray.pid
                )]
            );
            assert!(reaped_shortly(&mut child));
            let mut foreign = foreign;
            assert!(
                foreign.0.try_wait().unwrap().is_none(),
                "pid {foreign_pid} was not the loop's to end"
            );
        }

        /// Measured by the pre-PR review: `comm` is cut at 15 bytes, which can split a
        /// character, and a table that skips such a process reads its loop as stray.
        #[test]
        fn a_process_whose_comm_is_not_utf8_is_still_in_the_table() {
            let dir = tempfile::tempdir().unwrap();
            let exe = dir.path().join("aaaaaaaaaaaaaa\u{e9}");
            std::fs::copy("/bin/sleep", &exe).unwrap();
            let child = Reaped(spawn(
                &[exe.to_str().unwrap().to_string(), "3600".to_string()],
                0,
            ));
            let pid = child.0.id();
            wait_for_argv(pid, "3600");
            let comm = std::fs::read(format!("/proc/{pid}/comm")).unwrap();
            assert!(
                String::from_utf8(comm).is_err(),
                "the fixture must split the character"
            );
            assert!(live::read_table().unwrap().contains_key(&pid));
        }
    }

    /// A census that did not answer is not retried on every readiness poll.
    #[test]
    fn a_census_that_did_not_answer_cools_down() {
        assert_eq!(census_skip_until(100), 100 + CENSUS_COOLDOWN_SECS);
        assert_eq!(census_skip_until(u64::MAX), u64::MAX);
    }

    /// A read past its budget, or a second read while one is stuck, answers `None`
    /// and returns at once. That is what stands between a dead mount and the route.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_stuck_read_is_abandoned_and_not_joined() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let stuck = bounded_outcome(&FLAG, Duration::from_millis(50), move || {
            let _ = gate.recv();
            1
        })
        .await;
        assert_eq!(stuck, Err(Miss::Late), "past its budget");
        assert_eq!(
            bounded_outcome(&FLAG, Duration::from_secs(5), || 2).await,
            Err(Miss::Busy),
            "a stuck read is not joined, and says it was busy, not late"
        );
        release.send(()).unwrap();
        for _ in 0..100 {
            if !FLAG.load(Ordering::Acquire) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            bounded(&FLAG, Duration::from_secs(5), || 3).await,
            Some(3),
            "free again once it returns"
        );
    }

    /// A finished session with a real child must not pay for a table walk.
    #[test]
    fn a_shell_that_runs_no_wait_loop_is_refused_on_its_argv_alone() {
        assert!(wait_loop(&tool_argv("cargo test -p qontinui-runner")).is_err());
        assert!(wait_loop(&tool_argv("sleep 300")).is_err());
        assert!(wait_loop(&sv(&["rg", "x"])).is_err());
        assert!(wait_loop(&tool_argv("until ! pgrep -f x; do sleep 5; done")).is_ok());
    }

    /// A row on one frozen file, `/s/a.log`, with the given body.
    fn on_log(cmd: &str, body: &'static str) -> Verdict {
        one(
            cmd,
            &[],
            &[("/s/a.log", NOW - 3.0 * 3600.0, &[], Some(body))],
            &[],
            &[],
        )
    }

    /// `cat` of an empty file exits 0: such a loop has ended, and its shell may be in
    /// its post-loop `sleep`, on the way to real work.
    #[test]
    fn a_condition_with_no_grep_stage_is_not_judged() {
        for cmd in [
            "until cat /s/a.log >/dev/null; do sleep 5; done; sleep 600; ./deploy.sh",
            "until tail -n1 /s/a.log >/dev/null; do sleep 5; done; sleep 600",
            "until head -n1 /s/a.log >/dev/null; do sleep 5; done; sleep 600",
        ] {
            let v = on_log(cmd, "");
            assert!(!v.stray, "{cmd}: {v:?}");
            assert_eq!(v.why, "loop condition not judged", "{cmd}");
        }
    }

    #[test]
    fn tail_zero_and_a_carriage_return_are_not_judged() {
        let zero = "until tail -n0 /s/a.log | grep -q done; do sleep 5; done";
        assert!(!on_log(zero, "x\n").stray);
        let last = "until tail -n1 /s/a.log | grep -qx done; do sleep 5; done";
        assert!(!on_log(last, "50%\rdone\n").stray);
        assert!(on_log(last, "50%\nnot yet\n").stray);
    }

    /// Patterns Python's `re` refuses read as "not judged" there, so here too, even
    /// though the `regex` crate compiles them.
    #[test]
    fn a_regex_python_would_refuse_is_not_judged() {
        for pattern in [
            "a**", "^*", "a{2}{3}", "(?U)a", "*a", "a|*b", "(*a)", "a$*", r"\b*", r"\B?", r"a\b+",
            r"\<*",
        ] {
            assert_eq!(posix_re(pattern, true), None, "{pattern}");
        }
        for pattern in [
            "a*", "a+b?", "a{2}b", "(a|b)*c", "[*+?]*", r"a\*\*", "^a*$", r"\<foo", r"foo\>",
            r"^\<foo", r"\<a*\>", r"a\bb",
        ] {
            assert!(posix_re(pattern, true).is_some(), "{pattern}");
        }
        assert_eq!(
            on_log(
                "until grep -qE \"a{2}{3}\" /s/a.log; do sleep 5; done",
                "x\n"
            )
            .why,
            "loop condition not judged"
        );
    }
}
