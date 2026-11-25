//! The ccfg#2057 selftest cases (`stray_selftest`), ported: the measured
//! d2dd1423 set (8 self-matching loops on 2 patterns), the live-target, held-file
//! and bounded cases, and the shapes each rule must be able to fail on.

use super::*;

const NOW: u64 = 1_000_000_000;
const TDIR: &str = "/scratch/claude-1000/-s-proj/5b330eb3-5df7-459f-adf6-7927a533d510/tasks";
const GP: &str = "git push -u origin HEAD";
const CG: &str = "cargo-guard.sh test -- mcp::bridges";

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn proc_(ppid: u32, parts: &[&str]) -> StrayProc {
    StrayProc {
        ppid,
        argv: argv(parts),
    }
}

fn file(age_secs: u64, holders: &[u32], text: &str) -> FileFacts {
    FileFacts {
        mtime_secs: NOW - age_secs,
        holders: holders.iter().copied().collect(),
        text: Some(text.to_string()),
    }
}

fn old(text: &str) -> FileFacts {
    file(3 * 3600, &[], text)
}

fn base_facts() -> StrayFacts {
    let mut f = StrayFacts {
        my_uid: Some(1000),
        now_secs: NOW,
        self_pids: BTreeSet::from([30]),
        ..Default::default()
    };
    f.procs.insert(1, proc_(0, &["/sbin/init"]));
    f.procs.insert(10, proc_(1, &["/usr/bin/claude"]));
    f.procs.insert(30, proc_(1, &["python3", "cfs.py"]));
    f
}

struct Case {
    want: bool,
    class: Option<&'static str>,
    cmd: String,
    extra: Vec<(u32, StrayProc)>,
    files: Vec<(String, FileFacts)>,
    start: Vec<(u32, u64)>,
    kids: Vec<Vec<String>>,
    dirs: Option<Vec<String>>,
}

fn case(want: bool, class: Option<&'static str>, cmd: &str) -> Case {
    Case {
        want,
        class,
        cmd: cmd.to_string(),
        extra: vec![],
        files: vec![],
        start: vec![],
        kids: vec![argv(&["sleep", "15"])],
        dirs: None,
    }
}

impl Case {
    fn file(mut self, path: &str, f: FileFacts) -> Case {
        self.files.push((path.to_string(), f));
        self
    }
    fn kids(mut self, kids: &[&[&str]]) -> Case {
        self.kids = kids.iter().map(|k| argv(k)).collect();
        self
    }
    fn proc_(mut self, pid: u32, p: StrayProc) -> Case {
        self.extra.push((pid, p));
        self
    }
    fn start(mut self, pid: u32, t: u64) -> Case {
        self.start.push((pid, t));
        self
    }

    fn run(&self) -> StrayVerdict {
        let mut f = base_facts();
        f.procs.insert(
            40,
            StrayProc {
                ppid: 10,
                argv: tool_argv(&self.cmd),
            },
        );
        for (k, a) in self.kids.iter().enumerate() {
            f.procs.insert(
                41 + k as u32,
                StrayProc {
                    ppid: 40,
                    argv: a.clone(),
                },
            );
        }
        for (pid, p) in &self.extra {
            f.procs.insert(*pid, p.clone());
        }
        for (p, ff) in &self.files {
            f.files.insert(p.clone(), ff.clone());
        }
        f.start = self.start.iter().copied().collect();
        // Present dirs: the harness tasks dir, two ordinary `tasks` dirs, and each
        // file's own directory.
        f.dirs = match &self.dirs {
            Some(d) => d.iter().cloned().collect(),
            None => {
                let mut d: BTreeSet<String> = [
                    TDIR,
                    "/s/proj/tasks",
                    "/s/ci/tasks",
                    "/s/jobs/0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b/tasks",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect();
                d.extend(self.files.iter().map(|(p, _)| parent_dir(p).to_string()));
                d
            }
        };
        // An empty-argv kid is an exited, unreaped child.
        for (k, a) in self.kids.iter().enumerate() {
            if a.is_empty() {
                f.zombies.insert(41 + k as u32);
            }
        }
        stray_verdict(&f, 40, 15)
    }

    fn check(&self) {
        let got = self.run();
        assert_eq!(
            got.is_stray(),
            self.want,
            "{:?} -> {got:?}, want stray={}",
            self.cmd,
            self.want
        );
        if let (Some(want), Some(e)) = (self.class, got.evidence()) {
            assert_eq!(e.class_word(), want, "{:?} class", self.cmd);
        }
        if let (Some("c"), StrayVerdict::Work(n)) = (self.class, &got) {
            assert!(n.bounded, "{:?} should be a bounded wait", self.cmd);
        }
    }
}

fn task() -> String {
    format!("{TDIR}/brm8jg1o0.output")
}

fn d2_set() -> (StrayFacts, Vec<u32>) {
    let out =
        "/scratch/claude-1000/-s-proj/f116bbce-1e85-4828-a73f-06f0b90430b7/tasks/b1r1ys0pd.output";
    let loops: Vec<(u32, String)> = vec![
        (445646, format!("until ! pgrep -f \"{GP}\" >/dev/null; do sleep 15; done; cat {out}")),
        (464345, format!("until ! pgrep -f \"{GP}\" >/dev/null; do sleep 15; done; cat {out}")),
        (1873808, format!("until ! pgrep -f \"{GP}\" >/dev/null; do sleep 15; done; cat {out}")),
        (3135797, format!("until ! pgrep -f \"{GP}\" >/dev/null; do sleep 15; done; cat {out}")),
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
            format!("until ! pgrep -f \"{CG}\" >/dev/null; do sleep 15; done; cat /scratch/x/b2rb7udxa.output"),
        ),
    ];
    let mut f = base_facts();
    f.procs
        .insert(20, proc_(1, &["pty-holder", "--", "claude"]));
    f.procs.insert(
        21033,
        proc_(20, &["/usr/bin/claude", "--resume", "f116bbce"]),
    );
    f.procs.insert(50, proc_(1, &["vim", "notes.txt"]));
    let mut pids = Vec::new();
    for (p, c) in &loops {
        f.procs.insert(
            *p,
            StrayProc {
                ppid: 21033,
                argv: tool_argv(c),
            },
        );
        f.procs.insert(*p + 1, proc_(*p, &["sleep", "15"]));
        pids.push(*p);
    }
    // A loop's own probe caught mid-flight is a watcher, not a target.
    f.procs.insert(445648, proc_(445646, &["pgrep", "-f", GP]));
    (f, pids)
}

fn strays_of(f: &StrayFacts, shells: &[u32]) -> (Vec<u32>, Vec<(u32, String)>, Vec<u32>) {
    let mut work = Vec::new();
    let mut strays = Vec::new();
    let mut waits = Vec::new();
    for p in shells {
        match stray_verdict(f, *p, 15) {
            StrayVerdict::Stray(e) => strays.push((*p, e.class_word())),
            StrayVerdict::Work(n) => {
                work.push(*p);
                if let Some(w) = n.waits_on_live_pid {
                    waits.push(w);
                }
            }
        }
    }
    (work, strays, waits)
}

#[test]
fn the_measured_d2dd1423_set_is_eight_class_a_strays() {
    let (f, pids) = d2_set();
    let (work, strays, _) = strays_of(&f, &pids);
    assert!(work.is_empty(), "work: {work:?}");
    assert_eq!(strays.len(), 8);
    assert!(strays.iter().all(|(_, c)| c == "a"), "{strays:?}");
}

#[test]
fn one_stray_beside_a_real_cargo_test_leaves_the_cargo_shell_work() {
    let (mut f, mut pids) = d2_set();
    f.procs.insert(
        700,
        StrayProc {
            ppid: 21033,
            argv: tool_argv("cargo test -p qontinui-runner 2>&1 | tail -20"),
        },
    );
    f.procs
        .insert(701, proc_(700, &["cargo", "test", "-p", "qontinui-runner"]));
    pids.push(700);
    let (work, strays, _) = strays_of(&f, &pids);
    assert_eq!(work, vec![700]);
    assert_eq!(strays.len(), 8);
}

#[test]
fn a_self_matching_loop_whose_target_still_runs_waits_on_live_work() {
    // T46 cell 3: the loop whose real target runs is not a stray.
    let (mut f, pids) = d2_set();
    f.procs
        .insert(800, proc_(1, &["git", "push", "-u", "origin", "HEAD"]));
    let (work, strays, waits) = strays_of(&f, &pids);
    let on_gp: Vec<u32> = vec![445646, 464345, 1873808, 3135797, 3139942];
    assert_eq!(work.len(), on_gp.len());
    for p in on_gp {
        assert!(work.contains(&p));
    }
    assert_eq!(strays.len(), 3);
    assert!(waits.iter().all(|w| *w == 800));
    assert!(!waits.is_empty());
}

#[test]
fn an_unreadable_command_line_is_never_stray() {
    let mut f = base_facts();
    f.procs
        .insert(7, proc_(1, &["bash.exe", UNREADABLE_CMDLINE]));
    assert!(!stray_verdict(&f, 7, 15).is_stray());
    // No such pid.
    assert!(!stray_verdict(&f, 999, 15).is_stray());
    // Not a tool shell at all.
    f.procs.insert(
        8,
        proc_(1, &["bash", "-c", "until false; do sleep 1; done"]),
    );
    assert!(!stray_verdict(&f, 8, 15).is_stray());
}

#[test]
fn an_empty_argv_child_is_work_unless_it_is_a_zombie() {
    let c = case(
        true,
        Some("a"),
        "while ps aux | grep cargo >/dev/null; do sleep 5; done",
    )
    .kids(&[&[]]);
    c.check(); // zombie: runs nothing
    let mut f = base_facts();
    f.procs.insert(
        40,
        StrayProc {
            ppid: 10,
            argv: tool_argv("while ps aux | grep cargo >/dev/null; do sleep 5; done"),
        },
    );
    f.procs.insert(
        41,
        StrayProc {
            ppid: 40,
            argv: vec![],
        },
    );
    assert!(
        !stray_verdict(&f, 40, 15).is_stray(),
        "a live empty-argv process is work"
    );
}

#[test]
fn file_wait_cases() {
    let t = task();
    let twait = format!("until grep -qE 'exit=' {t} 2>/dev/null; do sleep 20; done; cat {t}");
    let census = "/s/census/cargo.log";
    // A tasks/<id>.output marker wait, nothing holds it, old.
    case(true, Some("b-file"), &twait)
        .file(&t, old("Compiling x\n"))
        .check();
    // A process holds it open.
    case(false, None, &twait)
        .file(&t, file(3 * 3600, &[950], "Compiling x\n"))
        .check();
    // The holder is the loop's own probe, caught mid-read.
    case(
        true,
        Some("b-file"),
        &format!("until grep -q DONE {t}; do sleep 5; done"),
    )
    .file(&t, file(3 * 3600, &[41], "Compiling x\n"))
    .kids(&[&["grep", "-q", "DONE", t.as_str()]])
    .check();
    // Changed inside the idle floor; 5 minutes is under the floor too.
    case(false, None, &twait)
        .file(&t, file(60, &[], "x\n"))
        .kids(&[])
        .check();
    case(
        false,
        None,
        &format!("until grep -q DONE {census}; do sleep 5; done"),
    )
    .file(census, file(5 * 60, &[], "x\n"))
    .kids(&[])
    .check();
    // Already holds.
    case(false, None, &twait)
        .file(&t, old("exit=0\n"))
        .kids(&[])
        .check();
    // A harness task file gone from a live tasks dir.
    case(true, Some("b-file"), &twait).kids(&[]).check();
    // A tasks/*.output outside the harness layout is an ordinary file: missing means "not yet".
    case(
        false,
        None,
        "until [ -f /s/proj/tasks/build.output ]; do sleep 5; done; ./deploy.sh",
    )
    .kids(&[])
    .check();
    case(
        false,
        None,
        "until grep -q DONE /s/ci/tasks/job42.output; do sleep 30; done; ./release.sh",
    )
    .kids(&[])
    .check();
    case(
        false,
        None,
        "until [ -f /s/jobs/0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b/tasks/out.output ]; do sleep 5; done; ./ship.sh",
    )
    .kids(&[])
    .check();
    // A missing non-task file does not exist YET.
    case(
        false,
        None,
        "until [ -f /s/build.done ]; do sleep 10; done; ./deploy.sh",
    )
    .kids(&[])
    .check();
    case(
        false,
        None,
        "while [ ! -f /s/build.done ]; do sleep 10; done; git push",
    )
    .kids(&[])
    .check();
    case(
        false,
        None,
        "until grep -q DONE /s/build.log 2>/dev/null; do sleep 10; done; echo ok",
    )
    .kids(&[])
    .check();
    // Pre-loop: inert assignments are fine, a detached producer is not.
    case(
        true,
        Some("b-file"),
        "S=/s; cd $S; export L=$S/x.log; echo watching; until grep -q DONE $L; do sleep 5; done",
    )
    .file("/s/x.log", old("x"))
    .check();
    case(
        false,
        None,
        "setsid -f bash -c 'sleep 3600; echo DONE >> /s/x.log'; until grep -q DONE /s/x.log; do sleep 5; done",
    )
    .file("/s/x.log", old("x"))
    .check();
    case(
        false,
        None,
        "X=$(setsid -f prod); until grep -q DONE /s/x.log; do sleep 5; done",
    )
    .file("/s/x.log", old("x"))
    .check();
    case(
        false,
        None,
        "L=/s/a.log; L=/s/b.log :; until grep -q DONE $L; do sleep 5; done",
    )
    .file("/s/a.log", old("x"))
    .file("/s/b.log", old("x"))
    .check();
    // Brace expansion and a changed IFS make the path unknown.
    case(
        false,
        None,
        "until grep -q DONE /s/{build,test}.log; do sleep 5; done",
    )
    .file("/s/{build,test}.log", old("x"))
    .check();
    case(
        false,
        None,
        "IFS=,; L=/s/a.log,/s/b; until grep -q DONE $L; do sleep 5; done",
    )
    .file("/s/a.log,/s/b", old("x"))
    .check();
    case(false, None, "until grep -q x $NOPE; do sleep 5; done").check();
    case(
        false,
        None,
        &format!("L='{t} x'; until grep -q DONE $L; do sleep 5; done"),
    )
    .file(&t, old("x"))
    .check();
    // A body that does more than wait.
    case(
        false,
        None,
        &format!("until ! pgrep -f \"{GP}\"; do sleep $(git push -u origin HEAD; echo 5); done"),
    )
    .check();
    case(
        false,
        None,
        "until ! pgrep -f \"zzq\"; do echo $(make deploy); sleep 5; done",
    )
    .check();
    case(
        false,
        None,
        "L=/s/a; until grep -q DONE $L; do printf -v L %s /s/b; sleep 5; done",
    )
    .file("/s/a", old("x"))
    .check();
    case(
        false,
        None,
        "until grep -q DONE /s/a.log; do echo tick >> /s/a.log; sleep 5; done",
    )
    .file("/s/a.log", old("x"))
    .check();
    case(
        true,
        Some("b-file"),
        "until grep -q DONE /s/a.log; do sleep 5 >/dev/null 2>&1; done",
    )
    .file("/s/a.log", old("x"))
    .check();
    // A job backgrounded before the loop may still be the writer.
    case(
        false,
        None,
        &format!("(sleep 9; echo x > {t}) & until [ -s {t} ]; do sleep 5; done"),
    )
    .file(&t, file(7200, &[], ""))
    .check();
    case(
        true,
        Some("b-file"),
        &format!("until [ -s {t} ]; do sleep 5; done"),
    )
    .file(&t, file(7200, &[], ""))
    .check();
    case(false, None, &format!("until [ -s {t} ]; do sleep 5; done"))
        .file(&t, file(7200, &[], "x"))
        .check();
    // The census's `|| ! pgrep -f` escape, with $L expanded from its literal assignment.
    let esc = format!(
        "L={census}; until grep -qE 'test result|^error|could not compile' $L || ! pgrep -f 'cargo test -- zz_census' >/dev/null; do sleep 15; done; tail -8 $L"
    );
    case(true, Some("a+b-file"), &esc)
        .file(census, old("Compiling x\n"))
        .check();
    case(false, None, &esc)
        .file(census, old("Compiling x\n"))
        .proc_(90, proc_(1, &["cargo", "test", "--", "zz_census"]))
        .check();
    // Work running under the loop, and a body that works.
    case(
        true,
        Some("b-file"),
        &format!("until grep -q DONE {t}; do sleep 5; done"),
    )
    .file(&t, old("x"))
    .kids(&[&["sleep", "5"], &["grep", "-q", "DONE", t.as_str()]])
    .check();
    case(
        false,
        None,
        &format!("until grep -q DONE {t}; do sleep 5; done"),
    )
    .file(&t, old("x"))
    .kids(&[&["sleep", "5"], &["cargo", "build"]])
    .check();
    case(
        false,
        None,
        &format!("until ! pgrep -f \"{GP}\"; do cargo build; sleep 5; done"),
    )
    .check();
}

#[test]
fn a_waits_for_a_file_to_go_is_not_proven_frozen() {
    for cmd in [
        "while [ -f /s/lock ]; do sleep 5; done; ./next.sh",
        "until [ ! -e /s/lock ]; do sleep 5; done; ./next.sh",
        "until ! [ -f /s/lock ]; do sleep 5; done; ./next.sh",
        "while [ -s /s/queue ]; do sleep 5; done",
        "while grep -q Compiling /s/lock; do sleep 5; done; ./next.sh",
        "until ! grep -q Compiling /s/lock; do sleep 5; done; ./next.sh",
        "while cat /s/lock >/dev/null 2>&1; do sleep 5; done; ./next.sh",
        "while tail -1 /s/lock | grep -q Compiling; do sleep 5; done",
        "while grep -q Compiling < /s/lock; do sleep 5; done",
        "while tail -1 /s/lock >/dev/null; do sleep 5; done",
        "while head -1 /s/lock >/dev/null; do sleep 5; done",
        "until grep -qv Compiling /s/state; do sleep 5; done; ./next.sh",
        "while ! grep -qv Compiling /s/state; do sleep 5; done",
    ] {
        case(false, None, cmd)
            .file("/s/lock", old("Compiling x"))
            .file("/s/queue", old("Compiling x"))
            .file("/s/state", old("Compiling x"))
            .kids(&[])
            .check();
    }
    // ... a harness task's own output file stays judged, and so does a wait for a line to appear.
    let t = task();
    case(
        true,
        Some("b-file"),
        &format!("while [ -f {t} ]; do sleep 5; done"),
    )
    .file(&t, old("x"))
    .kids(&[])
    .check();
    case(
        true,
        Some("b-file"),
        "until grep -q DONE /s/a.log; do sleep 5; done",
    )
    .file("/s/a.log", old("x"))
    .kids(&[])
    .check();
    case(
        true,
        Some("b-file"),
        "until tail -1 /s/a.log | grep -q DONE; do sleep 5; done",
    )
    .file("/s/a.log", old("x"))
    .kids(&[])
    .check();
    case(
        true,
        Some("b-file"),
        "until [ -s /s/empty ]; do sleep 5; done",
    )
    .file("/s/empty", file(7200, &[], ""))
    .kids(&[])
    .check();
    // A bare read's status is its exit code: not modelled.
    case(
        false,
        None,
        "until cat /s/empty >/dev/null; do sleep 1; done; sleep 600; ./deploy.sh",
    )
    .file("/s/empty", file(7200, &[], ""))
    .kids(&[])
    .check();
    case(
        false,
        None,
        &format!("until cat {t} >/dev/null; do sleep 1; done"),
    )
    .kids(&[])
    .check();
}

#[test]
fn bounded_waits_are_class_c_and_never_stray() {
    case(
        false,
        Some("c"),
        &format!("timeout 600 bash -c 'until ! pgrep -f \"{GP}\" >/dev/null; do sleep 5; done'"),
    )
    .check();
    case(
        false,
        Some("c"),
        &format!("i=0; until ! pgrep -f \"{GP}\"; do sleep 5; i=$((i+1)); done"),
    )
    .check();
    case(
        false,
        Some("c"),
        &format!("end=$(( $(date +%s) + 600 )); while [ $(date +%s) -lt $end ] && pgrep -f \"{GP}\"; do sleep 5; done"),
    )
    .check();
}

#[test]
fn self_excluding_patterns_are_real_waits_not_class_a() {
    case(
        false,
        None,
        "until ! pgrep -f '[g]it push' >/dev/null; do sleep 15; done",
    )
    .check();
    case(
        false,
        None,
        &format!("until ! pgrep -fx \"{GP}\" >/dev/null; do sleep 15; done"),
    )
    .check();
    case(
        false,
        None,
        &format!("until ! pgrep -fA \"{GP}\" >/dev/null; do sleep 15; done"),
    )
    .check();
    // No -f: pgrep reads names, not argv.
    case(false, None, "until ! pgrep cargo; do sleep 15; done").check();
    case(
        true,
        Some("a"),
        "while ps aux | grep cargo >/dev/null; do sleep 5; done",
    )
    .check();
    // `-v grep` drops the loop's own line too, so it ends with its target.
    case(
        false,
        None,
        "while ps aux | grep cargo | grep -v grep >/dev/null; do sleep 5; done",
    )
    .check();
    case(
        false,
        None,
        "while ps -p 4242 | grep -q 4242; do sleep 5; done",
    )
    .check();
    // `ps aux` prints the user column: a pattern on it still finds a live target.
    case(
        false,
        None,
        "while ps aux | grep -q \"1000.*cargo build\"; do sleep 5; done",
    )
    .proc_(90, proc_(1, &["cargo", "build"]))
    .check();
    case(
        true,
        Some("a"),
        "while ps aux | grep -q \"1000.*cargo build\"; do sleep 5; done",
    )
    .check();
    case(
        false,
        None,
        "while ps -eo args | grep -q \"^cargo build\"; do sleep 5; done",
    )
    .check();
}

#[test]
fn kill_zero_waits_on_a_reused_pid_are_strays_and_on_the_same_process_are_not() {
    case(
        true,
        Some("b-pid"),
        "while kill -0 4242 2>/dev/null; do sleep 5; done",
    )
    .proc_(4242, proc_(1, &["daemon"]))
    .start(40, 100)
    .start(4242, 200)
    .check();
    case(
        false,
        None,
        "while kill -0 4242 2>/dev/null; do sleep 5; done",
    )
    .proc_(4242, proc_(1, &["make"]))
    .start(40, 100)
    .start(4242, 50)
    .check();
    // A gone pid ends a while-loop.
    case(
        false,
        None,
        "while kill -0 4243 2>/dev/null; do sleep 5; done",
    )
    .start(40, 100)
    .check();
}

#[test]
fn not_one_loop_and_unmodelled_syntax_are_work() {
    case(
        false,
        None,
        &format!("until ! pgrep -f \"{GP}\"; do sleep 5; done & wait"),
    )
    .check();
    case(false, None, "cat <<EOF\nx\nEOF").check();
    case(false, None, "echo hi").check();
}

#[test]
fn a_probe_newer_than_the_table_is_not_a_writer_but_another_parent_is() {
    let census = "/s/census/cargo.log";
    let cmd = format!("until grep -q DONE {census}; do sleep 5; done");
    let mut f = base_facts();
    f.procs.insert(
        40,
        StrayProc {
            ppid: 10,
            argv: tool_argv(&cmd),
        },
    );
    f.files
        .insert(census.to_string(), file(3 * 3600, &[77], "x\n"));
    f.live_ppids.insert(77, 40);
    assert!(
        stray_verdict(&f, 40, 15).is_stray(),
        "a probe under the loop is no writer"
    );
    f.live_ppids.insert(77, 99);
    assert!(
        !stray_verdict(&f, 40, 15).is_stray(),
        "a holder under another parent is a writer"
    );
}

#[test]
fn a_low_idle_min_never_lowers_the_frozen_file_proof() {
    let census = "/s/census/cargo.log";
    let mut f = base_facts();
    f.procs.insert(
        40,
        StrayProc {
            ppid: 10,
            argv: tool_argv(&format!("until grep -q DONE {census}; do sleep 5; done")),
        },
    );
    f.files.insert(census.to_string(), file(300, &[], "x\n"));
    for floor in [0, 5] {
        assert!(
            !stray_verdict(&f, 40, floor).is_stray(),
            "--idle-min {floor}"
        );
    }
}

#[test]
fn a_task_file_whose_tasks_dir_is_gone_is_not_known_to_be_the_harness() {
    let t = task();
    let mut c = case(
        false,
        None,
        &format!("until grep -qE 'exit=' {t} 2>/dev/null; do sleep 20; done; cat {t}"),
    );
    c.dirs = Some(vec![]);
    c.kids = vec![];
    c.check();
}

#[test]
fn tool_command_round_trips_quotes() {
    let cmd = "echo 'a b' \"$X\"";
    assert_eq!(tool_command(&tool_argv(cmd)).as_deref(), Some(cmd));
}

#[test]
fn posix_patterns() {
    let m = |pat: &str, ere: bool, line: &str| -> Option<bool> {
        Regex::posix(pat, ere, false, false).map(|r| r.is_match(line).unwrap())
    };
    assert_eq!(
        m(r"test rc=\|clippy rc=[1-9]", false, "clippy rc=2"),
        Some(true)
    );
    assert_eq!(m(r"error(\[|:)", true, "error[E0425]"), Some(true));
    assert_eq!(m("a+", false, "aa"), Some(false));
    assert_eq!(m("a+", false, "a+"), Some(true));
    assert_eq!(m("[[:alpha:]]", true, "x"), None);
    assert_eq!(m(r"(a|b)c{2}", true, "xbcc"), Some(true));
    assert_eq!(m("^cargo build", true, "cargo build -p x"), Some(true));
    assert_eq!(m("^cargo build", true, "x cargo build"), Some(false));
    assert_eq!(m(r"\<git\>", true, "run git push"), Some(true));
    assert_eq!(m(r"\<git\>", true, "digit"), Some(false));
    assert_eq!(m(r"(\1)", true, "x"), None);
}

#[test]
fn a_pathological_pattern_is_not_judged_rather_than_matched_wrongly() {
    let r = Regex::posix("(a*)*b", true, false, false).unwrap();
    let line = "a".repeat(64);
    // Either a correct answer or an explicit give-up, never a hang.
    let _ = r.is_match(&line);
}

#[test]
fn harness_task_file_layout() {
    assert!(is_harness_task_file(&task()));
    assert!(!is_harness_task_file("/s/proj/tasks/build.output"));
    assert!(!is_harness_task_file(
        "/s/jobs/0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b/tasks/out.output"
    ));
}
