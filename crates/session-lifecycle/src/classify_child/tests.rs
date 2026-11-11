//! The measured fixtures (`fixtures/classify_child/`, recorded from real hosts
//! before this classifier was written) and the synthetic cases each rule needs
//! to be able to fail.

use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    platform: String,
    claude_pid: u32,
    table: Vec<FixtureRow>,
}

#[derive(Deserialize, Clone)]
struct FixtureRow {
    pid: u32,
    ppid: u32,
    image: String,
    start: u64,
    /// The command line as the capture recorded it. Windows: the raw line, which
    /// is what the classifier reads. Linux: `ps -o args=` — the argv joined with
    /// single spaces — kept as measured, but never read: see `argv`.
    cmdline: Option<String>,
    /// Linux only: the argv as `/proc/<pid>/cmdline` separates it, the form the
    /// classifier reads there.
    #[serde(default)]
    argv: Option<Vec<String>>,
}

/// Embedded at compile time, so the test binary reads no file at run time and
/// its working directory (CI runs every binary from `src-tauri`) is irrelevant.
fn fixture(name: &str) -> Fixture {
    let text = match name {
        "linux-shim" => include_str!("../../fixtures/classify_child/linux-shim.json"),
        "linux-shim-plus-tool-shell" => {
            include_str!("../../fixtures/classify_child/linux-shim-plus-tool-shell.json")
        }
        "linux-shim-plus-owned-run-python" => {
            include_str!("../../fixtures/classify_child/linux-shim-plus-owned-run-python.json")
        }
        "windows-shim-chain" => {
            include_str!("../../fixtures/classify_child/windows-shim-chain.json")
        }
        "windows-shim-plus-tool-shells" => {
            include_str!("../../fixtures/classify_child/windows-shim-plus-tool-shells.json")
        }
        "windows-shim-plus-direct-conhost" => {
            include_str!("../../fixtures/classify_child/windows-shim-plus-direct-conhost.json")
        }
        other => panic!("no fixture {other}"),
    };
    serde_json::from_str(text).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

const ALL_FIXTURES: [&str; 6] = [
    "linux-shim",
    "linux-shim-plus-tool-shell",
    "linux-shim-plus-owned-run-python",
    "windows-shim-chain",
    "windows-shim-plus-tool-shells",
    "windows-shim-plus-direct-conhost",
];

fn platform_of(f: &Fixture) -> Platform {
    match f.platform.as_str() {
        "linux" => Platform::Linux,
        "windows" => Platform::Windows,
        other => panic!("fixture platform {other}"),
    }
}

fn table_of(rows: &[FixtureRow]) -> ProcTable {
    let mut t = ProcTable::new(true);
    for r in rows {
        t.insert(
            r.pid,
            ProcRow {
                ppid: r.ppid,
                image: Some(r.image.clone()),
                start: Some(r.start),
            },
        );
    }
    t
}

/// Each row's command line in its platform's [`Argv`] form: the NUL-separated
/// `argv` on Linux, the raw `cmdline` on Windows. A row without one is absent.
fn argv_of(platform: Platform, rows: &[FixtureRow]) -> HashMap<u32, Argv> {
    rows.iter()
        .filter_map(|r| {
            let argv = match platform {
                Platform::Linux => Argv::Elements(r.argv.clone()?),
                _ => Argv::Line(r.cmdline.clone()?),
            };
            Some((r.pid, argv))
        })
        .collect()
}

/// The fixture classified with one table serving as both reads.
fn run(name: &str) -> SessionVerdict {
    let f = fixture(name);
    let t = table_of(&f.table);
    let p = platform_of(&f);
    classify_session(p, f.claude_pid, &t, Some(&argv_of(p, &f.table)), &t)
}

fn shim_pid(f: &Fixture) -> u32 {
    let kids: Vec<&FixtureRow> = f.table.iter().filter(|r| r.ppid == f.claude_pid).collect();
    kids.iter()
        .find(|r| {
            let i = r.image.to_ascii_lowercase();
            (i.starts_with("python")
                && r.argv
                    .as_ref()
                    .is_some_and(|a| a.iter().any(|x| x.ends_with("coord-mcp-shim.py"))))
                || i == "cmd.exe"
        })
        .map(|r| r.pid)
        .expect("fixture has a shim child")
}

// --- the fixtures themselves ---------------------------------------------------

#[test]
fn every_linux_row_carries_an_argv_that_joins_back_to_what_was_measured() {
    // The Linux fixtures recorded `ps -o args=`, a space-joined line. Every row
    // the classifier reads carries its argv separately, and that argv must be
    // the measured line cut at argument boundaries — never different text.
    for name in ALL_FIXTURES {
        let f = fixture(name);
        if platform_of(&f) != Platform::Linux {
            continue;
        }
        for r in f.table.iter().filter(|r| r.pid != f.claude_pid) {
            let argv = r
                .argv
                .as_ref()
                .unwrap_or_else(|| panic!("{name}: pid {} has no argv", r.pid));
            if r.image == "next-server" {
                // A retitled node process: its recorded line carries `ps`'s
                // comm column tail (see the fixture's provenance).
                continue;
            }
            assert_eq!(
                Some(argv.join(" ")),
                r.cmdline,
                "{name}: pid {}'s argv is not the measured line",
                r.pid
            );
        }
    }
}

// --- measured fixtures --------------------------------------------------------

#[test]
fn linux_measured_shim_alone_is_admitted() {
    let v = run("linux-shim");
    assert!(v.admit, "{v:?}");
    assert_eq!(v.classes(), "shim_or_pump=1");
    assert_eq!(v.children[0].chain, Some(CHAIN_LINUX_SHIM));
    assert_eq!(v.word(), "would admit");
}

#[test]
fn windows_measured_cmd_python3_python_chain_is_admitted() {
    let v = run("windows-shim-chain");
    assert!(v.admit, "{v:?}");
    assert_eq!(v.classes(), "shim_or_pump=1");
    assert_eq!(v.children[0].chain, Some(CHAIN_WINDOWS_CMD_SHIM));
}

#[test]
fn linux_a_tool_shell_beside_the_shim_refuses() {
    let v = run("linux-shim-plus-tool-shell");
    assert!(!v.admit);
    assert_eq!(v.classes(), "work=1 shim_or_pump=1");
    assert_eq!(v.word(), "would refuse");
}

#[test]
fn linux_a_second_python_that_is_not_the_shim_is_work() {
    // python3 owned-run.py -> a `next dev` server: same image as the shim, a
    // different script and a subtree. Image alone must not admit it.
    let v = run("linux-shim-plus-owned-run-python");
    assert!(!v.admit);
    assert_eq!(v.classes(), "work=1 shim_or_pump=1");
}

#[test]
fn windows_tool_shells_beside_the_shim_refuse() {
    let v = run("windows-shim-plus-tool-shells");
    assert!(!v.admit);
    assert_eq!(v.classes(), "work=5 shim_or_pump=1");
}

#[test]
fn windows_a_direct_conhost_child_is_not_admitted() {
    // conhost.exe is admitted only INSIDE the measured cmd chain.
    let v = run("windows-shim-plus-direct-conhost");
    assert!(!v.admit);
    let conhost = v
        .children
        .iter()
        .find(|c| c.reason.starts_with("conhost"))
        .expect("the direct conhost child");
    assert_eq!(conhost.class, ChildClass::Work);
}

#[test]
fn a_chain_is_admitted_only_on_the_platform_it_was_measured_on() {
    for (name, wrong) in [
        ("linux-shim", Platform::Windows),
        ("windows-shim-chain", Platform::Linux),
    ] {
        let f = fixture(name);
        let t = table_of(&f.table);
        // The fixture's command lines in the WRONG platform's own form, so the
        // matcher runs and it is that platform's chain that refuses — not an
        // unreadable command line.
        let argv: HashMap<u32, Argv> = f
            .table
            .iter()
            .filter_map(|r| {
                let a = match wrong {
                    Platform::Linux => Argv::Elements(
                        r.argv
                            .clone()
                            .or_else(|| r.cmdline.as_deref().map(split_windows_args))?,
                    ),
                    _ => Argv::Line(r.cmdline.clone()?),
                };
                Some((r.pid, a))
            })
            .collect();
        let v = classify_session(wrong, f.claude_pid, &t, Some(&argv), &t);
        assert!(!v.admit, "{name} admitted on {wrong:?}: {v:?}");
        assert_eq!(v.classes(), "work=1", "{name} on {wrong:?}: {v:?}");
        let v = classify_session(Platform::Other, f.claude_pid, &t, Some(&argv), &t);
        assert!(!v.admit, "{name} admitted on Other");
    }
}

#[test]
fn an_argv_not_in_its_platforms_form_is_unknown_never_coerced() {
    // A space-joined line on Linux is exactly the collapsed form the argv
    // elements replaced; a pre-split argv on Windows is not the PEB's line.
    // Neither is converted into the other: both are unreadable.
    for name in ["linux-shim", "windows-shim-chain"] {
        let f = fixture(name);
        let p = platform_of(&f);
        let t = table_of(&f.table);
        let swapped: HashMap<u32, Argv> = argv_of(p, &f.table)
            .into_iter()
            .map(|(pid, a)| {
                let other = match a {
                    Argv::Elements(args) => Argv::Line(args.join(" ")),
                    Argv::Line(line) => Argv::Elements(split_windows_args(&line)),
                };
                (pid, other)
            })
            .collect();
        let v = classify_child(p, shim_pid(&f), &t, Some(&swapped), &t);
        assert_eq!(v.class, ChildClass::Unknown, "{name}: {v:?}");
        assert!(v.reason.contains("command line is unreadable"), "{v:?}");
    }
}

// --- binding: pid + creation time, re-confirmed --------------------------------

fn mutate_fresh(name: &str, edit: impl FnOnce(&mut Vec<FixtureRow>, u32)) -> ChildVerdict {
    let f = fixture(name);
    let p = platform_of(&f);
    let shim = shim_pid(&f);
    let first = table_of(&f.table);
    let mut rows = f.table.clone();
    edit(&mut rows, shim);
    let fresh = table_of(&rows);
    classify_child(p, shim, &first, Some(&argv_of(p, &f.table)), &fresh)
}

#[test]
fn a_pid_reused_between_the_reads_is_unknown() {
    for name in ["linux-shim", "windows-shim-chain"] {
        // The deepest member is the one a reuse is least visible on.
        let v = mutate_fresh(name, |rows, _| {
            let last = rows.last_mut().unwrap();
            last.start += 10_000_000;
        });
        assert_eq!(v.class, ChildClass::Unknown, "{name}: {v:?}");
        assert!(v.reason.contains("reused"), "{v:?}");
    }
}

#[test]
fn a_member_that_left_before_the_fresh_read_is_unknown() {
    let v = mutate_fresh("windows-shim-chain", |rows, _| {
        rows.pop();
    });
    assert_eq!(v.class, ChildClass::Unknown);
    assert!(v.reason.contains("left"), "{v:?}");
}

#[test]
fn a_member_that_arrived_before_the_fresh_read_is_unknown() {
    let v = mutate_fresh("linux-shim", |rows, shim| {
        rows.push(FixtureRow {
            pid: 999_999,
            ppid: shim,
            image: "bash".into(),
            start: 1,
            cmdline: None,
            argv: None,
        });
    });
    assert_eq!(v.class, ChildClass::Unknown);
    assert!(v.reason.contains("membership"), "{v:?}");
}

#[test]
fn a_member_whose_image_changed_between_the_reads_is_unknown() {
    // A pid reused inside one clock tick by a different program keeps its
    // start and parent; only the image tells.
    let v = mutate_fresh("linux-shim", |rows, shim| {
        rows.iter_mut().find(|r| r.pid == shim).unwrap().image = "node".into();
    });
    assert_eq!(v.class, ChildClass::Unknown);
    assert!(v.reason.contains("image"), "{v:?}");
}

#[test]
fn a_shim_script_outside_the_fleet_config_repo_is_not_the_measured_chain() {
    let v = mutate_chain("linux-shim", |r| {
        let row = shim_row(r);
        let argv = row.argv.as_mut().unwrap();
        argv[1] = argv[1].replace("/qontinui-claude-config/scripts/", "/tmp/scripts/");
    });
    assert_eq!(v.class, ChildClass::Work);
    assert!(v.reason.contains("the script is not"), "{v:?}");
}

#[test]
fn a_member_reparented_between_the_reads_is_unknown() {
    let v = mutate_fresh("windows-shim-chain", |rows, _| {
        rows.last_mut().unwrap().ppid = 1;
    });
    assert_eq!(v.class, ChildClass::Unknown);
}

#[test]
fn an_unknown_creation_time_binds_nothing() {
    for zero in [None, Some(0)] {
        let f = fixture("linux-shim");
        let shim = shim_pid(&f);
        let mut t = table_of(&f.table);
        let row = t.row(shim).unwrap().clone();
        t.insert(shim, ProcRow { start: zero, ..row });
        let argv = argv_of(Platform::Linux, &f.table);
        let v = classify_child(Platform::Linux, shim, &t, Some(&argv), &t);
        assert_eq!(v.class, ChildClass::Unknown, "{zero:?}");
    }
}

#[test]
fn a_stale_table_is_unknown_and_refuses_the_session() {
    let f = fixture("linux-shim");
    let mut t = table_of(&f.table);
    t.fresh = false;
    let fresh = table_of(&f.table);
    let shim = shim_pid(&f);
    let argv = argv_of(Platform::Linux, &f.table);
    assert_eq!(
        classify_child(Platform::Linux, shim, &t, Some(&argv), &fresh).class,
        ChildClass::Unknown
    );
    assert_eq!(
        classify_child(Platform::Linux, shim, &fresh, Some(&argv), &t).class,
        ChildClass::Unknown
    );
    assert!(!classify_session(Platform::Linux, f.claude_pid, &t, Some(&argv), &fresh).admit);
}

#[test]
fn a_reused_claude_refuses_the_session() {
    let f = fixture("linux-shim");
    let first = table_of(&f.table);
    let mut rows = f.table.clone();
    rows[0].start += 1;
    let fresh = table_of(&rows);
    let argv = argv_of(Platform::Linux, &f.table);
    let v = classify_session(Platform::Linux, f.claude_pid, &first, Some(&argv), &fresh);
    assert!(!v.admit);
    assert_eq!(v.classes(), "claude unbound");
}

#[test]
fn a_child_that_appeared_under_claude_between_the_reads_refuses() {
    let f = fixture("linux-shim");
    let first = table_of(&f.table);
    let mut rows = f.table.clone();
    rows.push(FixtureRow {
        pid: 424_242,
        ppid: f.claude_pid,
        image: "bash".into(),
        start: 5,
        cmdline: None,
        argv: None,
    });
    let fresh = table_of(&rows);
    let argv = argv_of(Platform::Linux, &f.table);
    let v = classify_session(Platform::Linux, f.claude_pid, &first, Some(&argv), &fresh);
    assert!(!v.admit, "{v:?}");
}

// --- unreadable is UNKNOWN ----------------------------------------------------

#[test]
fn a_failed_command_line_read_is_unknown_never_admitted() {
    for name in ["linux-shim", "windows-shim-chain"] {
        let f = fixture(name);
        let t = table_of(&f.table);
        let v = classify_session(platform_of(&f), f.claude_pid, &t, None, &t);
        assert!(!v.admit);
        assert_eq!(v.classes(), "unknown=1", "{name}");
    }
}

#[test]
fn one_members_missing_command_line_is_unknown() {
    // A pid absent from a `Some` command-line map is UNKNOWN.
    for name in ["linux-shim", "windows-shim-chain"] {
        let f = fixture(name);
        let p = platform_of(&f);
        let t = table_of(&f.table);
        let mut argv = argv_of(p, &f.table);
        let deepest = f.table.last().unwrap().pid;
        argv.remove(&deepest);
        let v = classify_child(p, shim_pid(&f), &t, Some(&argv), &t);
        assert_eq!(v.class, ChildClass::Unknown, "{name}: {v:?}");
    }
}

#[test]
fn an_empty_argv_is_unknown() {
    // An empty read (a kernel thread, a zombie) never reaches the map in the
    // runner; an argv of nothing but empty elements says nothing either.
    for empty in [Vec::new(), vec![String::new()], vec![String::new(); 3]] {
        let f = fixture("linux-shim");
        let t = table_of(&f.table);
        let shim = shim_pid(&f);
        let argv = HashMap::from([(shim, Argv::Elements(empty.clone()))]);
        let v = classify_child(Platform::Linux, shim, &t, Some(&argv), &t);
        assert_eq!(v.class, ChildClass::Unknown, "{empty:?}: {v:?}");
    }
}

#[test]
fn an_unreadable_image_is_unknown() {
    let f = fixture("windows-shim-chain");
    let mut t = table_of(&f.table);
    let deepest = f.table.last().unwrap().pid;
    let row = t.row(deepest).unwrap().clone();
    t.insert(deepest, ProcRow { image: None, ..row });
    let argv = argv_of(Platform::Windows, &f.table);
    let v = classify_child(Platform::Windows, shim_pid(&f), &t, Some(&argv), &t);
    assert_eq!(v.class, ChildClass::Unknown);
}

// --- Linux argv is compared element by element --------------------------------

/// The measured shim argv with `edit` applied to its elements.
fn linux_shim_with_argv(edit: impl FnOnce(&mut Vec<String>)) -> ChildVerdict {
    mutate_chain("linux-shim", |r| edit(shim_row(r).argv.as_mut().unwrap()))
}

#[test]
fn linux_argvs_that_join_to_the_measured_line_are_not_the_shim() {
    // Each of these, joined with spaces and split again on whitespace, reads
    // exactly as the measured shim did — the collapse a rejoined `/proc` read
    // produced. Element by element, none is the measured argv.
    type Edit = fn(&mut Vec<String>);
    let collapsed: [(&str, Edit); 4] = [
        ("`--credential <path>` as ONE argument", |a| {
            let path = a.pop().unwrap();
            let flag = a.pop().unwrap();
            a.push(format!("{flag} {path}"));
        }),
        ("the interpreter and the script as ONE argument", |a| {
            let script = a.remove(1);
            a[0] = format!("{} {script}", a[0]);
        }),
        ("an empty argument after the credential", |a| {
            a.push(String::new())
        }),
        ("an empty argument before the script", |a| {
            a.insert(1, String::new())
        }),
    ];
    let measured = fixture("linux-shim")
        .table
        .iter()
        .find_map(|r| r.argv.clone())
        .unwrap();
    let words = |a: &[String]| -> Vec<String> {
        a.join(" ").split_whitespace().map(str::to_string).collect()
    };
    for (what, edit) in collapsed {
        let mut mutated = measured.clone();
        edit(&mut mutated);
        assert_ne!(mutated, measured, "{what}: the mutant must differ");
        assert_eq!(
            words(&mutated),
            words(&measured),
            "{what}: the mutant must collapse to the measured line when rejoined"
        );

        let v = linux_shim_with_argv(edit);
        assert_ne!(v.class, ChildClass::ShimOrPump, "{what} admitted: {v:?}");
        assert_eq!(v.class, ChildClass::Work, "{what}: {v:?}");
    }
}

#[test]
fn linux_a_credential_path_holding_a_space_is_one_argument() {
    // The flip side of element comparison: a path WITH a space is still one
    // path, which a whitespace split would have broken in two.
    let v = linux_shim_with_argv(|a| {
        *a.last_mut().unwrap() = "/home/x/my creds/0123456789abcdef.json".into();
    });
    assert_eq!(v.class, ChildClass::ShimOrPump, "{v:?}");
}

// --- the image rule settles a busy tool shell -----------------------------------

/// A table: `claude` 10 → `shell` 11 → `n` direct `sleep` children.
fn busy_shell(shell: &str, n: u32) -> ProcTable {
    let mut t = ProcTable::new(true);
    let row = |ppid: u32, image: &str, start: u64| ProcRow {
        ppid,
        image: Some(image.to_string()),
        start: Some(start),
    };
    t.insert(10, row(1, "claude", 5));
    t.insert(11, row(10, shell, 6));
    for i in 0..n {
        t.insert(100 + i, row(11, "sleep", 7));
    }
    t
}

#[test]
fn a_busy_tool_shell_is_work_not_unknown() {
    let none = HashMap::new();
    for (platform, shell) in [(Platform::Linux, "bash"), (Platform::Windows, "bash.exe")] {
        // Past the walk's bound: the subtree cannot be walked at all.
        let big = busy_shell(shell, 70);
        let v = classify_child(platform, 11, &big, Some(&none), &big);
        assert_eq!(v.class, ChildClass::Work, "{platform:?} large: {v:?}");
        assert!(v.reason.contains("tool shell"), "{v:?}");

        // Its subtree changing between the reads (a command started).
        let first = busy_shell(shell, 2);
        let fresh = busy_shell(shell, 3);
        let v = classify_child(platform, 11, &first, Some(&none), &fresh);
        assert_eq!(v.class, ChildClass::Work, "{platform:?} changing: {v:?}");

        // It still never admits, and a stale table still reads nothing.
        let mut stale = busy_shell(shell, 2);
        stale.fresh = false;
        let v = classify_child(platform, 11, &stale, Some(&none), &first);
        assert_eq!(v.class, ChildClass::Unknown, "{platform:?} stale: {v:?}");
    }
    // A Windows cmd is the shim's wrapper too, so the image rule leaves it to
    // the chain, and a changing subtree there is UNKNOWN, not Work.
    let first = busy_shell("cmd.exe", 2);
    let fresh = busy_shell("cmd.exe", 3);
    let v = classify_child(Platform::Windows, 11, &first, Some(&none), &fresh);
    assert_eq!(v.class, ChildClass::Unknown, "windows cmd: {v:?}");
    // Off Windows `cmd` is only a shell name.
    let v = classify_child(Platform::Linux, 11, &first, Some(&none), &fresh);
    assert_eq!(v.class, ChildClass::Work, "linux cmd: {v:?}");
}

// --- the chain must match WHOLE ------------------------------------------------

fn mutate_chain(name: &str, edit: impl FnOnce(&mut Vec<FixtureRow>)) -> ChildVerdict {
    let f = fixture(name);
    let p = platform_of(&f);
    let shim = shim_pid(&f);
    let mut rows = f.table.clone();
    edit(&mut rows);
    let t = table_of(&rows);
    classify_child(p, shim, &t, Some(&argv_of(p, &rows)), &t)
}

fn row_by_image<'a>(rows: &'a mut [FixtureRow], image: &str) -> &'a mut FixtureRow {
    rows.iter_mut().find(|r| r.image == image).unwrap()
}

/// The Linux fixture's shim row (the python3 child whose argv runs the shim).
fn shim_row(rows: &mut [FixtureRow]) -> &mut FixtureRow {
    rows.iter_mut()
        .find(|r| {
            r.argv
                .as_ref()
                .is_some_and(|a| a.iter().any(|x| x.ends_with("coord-mcp-shim.py")))
        })
        .unwrap()
}

/// Replace a Windows row's command line through `edit`.
fn edit_line(rows: &mut [FixtureRow], image: &str, edit: impl FnOnce(&str) -> String) {
    let row = row_by_image(rows, image);
    row.cmdline = Some(edit(row.cmdline.as_deref().unwrap()));
}

type RowEdit = fn(&mut Vec<FixtureRow>);

/// Each mutant breaks ONE rule of a measured chain and names the refusal that
/// rule gives, so the test fails both when a mutant is admitted and when some
/// other rule is what stopped it.
fn assert_each_refused_by_its_rule(name: &str, mutants: &[(&str, &str, RowEdit)]) {
    for (what, refusal, edit) in mutants {
        let v = mutate_chain(name, edit);
        assert_ne!(
            v.class,
            ChildClass::ShimOrPump,
            "{name}: {what} still admitted: {v:?}"
        );
        assert_eq!(v.class, ChildClass::Work, "{name}: {what}: {v:?}");
        assert!(
            v.reason.contains(refusal),
            "{name}: {what} was refused by another rule than `{refusal}`: {}",
            v.reason
        );
    }
}

#[test]
fn the_linux_chain_is_mutation_partnered() {
    // Every rule the Linux matcher applies, each broken alone, must stop the
    // admit, and be the rule that stops it.
    let mutants: [(&str, &str, RowEdit); 8] = [
        ("another script", "the script is not", |r| {
            shim_row(r).argv.as_mut().unwrap()[1] = "/tmp/other.py".into()
        }),
        ("-c instead of a script", "the script is not", |r| {
            *shim_row(r).argv.as_mut().unwrap() = vec![
                "/usr/bin/python3".into(),
                "-c".into(),
                "import os".into(),
                "/x/qontinui-claude-config/scripts/coord-mcp-shim.py".into(),
            ]
        }),
        (
            "an extra flag",
            "neither none nor `--credential <path>`",
            |r| shim_row(r).argv.as_mut().unwrap().push("--verbose".into()),
        ),
        (
            "a flag in place of the path",
            "neither none nor `--credential <path>`",
            |r| *shim_row(r).argv.as_mut().unwrap().last_mut().unwrap() = "--verbose".into(),
        ),
        (
            "a non-python interpreter",
            "argv[0] is not a python interpreter",
            |r| shim_row(r).argv.as_mut().unwrap()[0] = "/usr/bin/node".into(),
        ),
        (
            "a non-python image",
            "the image is not a python interpreter",
            |r| row_by_image(r, "python3").image = "node".into(),
        ),
        (
            "a script that only resembles the shim",
            "the script is not",
            |r| {
                let argv = shim_row(r).argv.as_mut().unwrap();
                argv[1] = argv[1].replace("coord-mcp-shim.py", "coord-mcp-shim.py.bak");
            },
        ),
        (
            "a grandchild under the shim",
            "the subtree is not the shim alone",
            |r| {
                let shim = shim_row(r).pid;
                r.push(FixtureRow {
                    pid: 77,
                    ppid: shim,
                    image: "sleep".into(),
                    start: 9,
                    cmdline: Some("sleep 10".into()),
                    argv: Some(vec!["sleep".into(), "10".into()]),
                });
            },
        ),
    ];
    assert_each_refused_by_its_rule("linux-shim", &mutants);
}

#[test]
fn the_windows_chain_is_mutation_partnered() {
    // Mutants that change the subtree's SIZE are stopped by the four-member
    // check before any other rule runs.
    let resized: [(&str, &str, RowEdit); 4] = [
        ("no conhost", "four members", |r| {
            r.retain(|x| x.image != "conhost.exe")
        }),
        ("no python3 hop", "four members", |r| {
            // cmd -> python directly (the hop removed, python reparented).
            let cmd = row_by_image(r, "cmd.exe").pid;
            r.retain(|x| x.image != "python3.exe");
            row_by_image(r, "python.exe").ppid = cmd;
        }),
        ("a second conhost beside the chain", "four members", |r| {
            let cmd = row_by_image(r, "cmd.exe").pid;
            let mut extra = row_by_image(r, "conhost.exe").clone();
            extra.pid = 55;
            extra.ppid = cmd;
            r.push(extra);
        }),
        ("a child under the real interpreter", "four members", |r| {
            let py = row_by_image(r, "python.exe").pid;
            r.push(FixtureRow {
                pid: 66,
                ppid: py,
                image: "git.exe".into(),
                start: 3,
                cmdline: Some("git status".into()),
                argv: None,
            });
        }),
    ];
    assert_each_refused_by_its_rule("windows-shim-chain", &resized);

    // Every other rule, each broken alone with the subtree kept at FOUR
    // members, so the size check passes and that rule is the one that refuses.
    let four: [(&str, &str, RowEdit); 13] = [
        ("cmd without /s", "is not `cmd.exe /d /s /c", |r| {
            edit_line(r, "cmd.exe", |l| l.replacen(" /s ", " ", 1))
        }),
        ("cmd /k instead of /c", "is not `cmd.exe /d /s /c", |r| {
            edit_line(r, "cmd.exe", |l| l.replacen(" /c ", " /k ", 1))
        }),
        ("cmd running another script", "the script is not", |r| {
            edit_line(r, "cmd.exe", |l| l.replace("coord-mcp-shim.py", "evil.py"))
        }),
        (
            "cmd running another interpreter",
            "does not start python3",
            |r| {
                edit_line(r, "cmd.exe", |l| {
                    l.replace(r"WindowsApps\python3.exe", r"WindowsApps\node.exe")
                })
            },
        ),
        (
            "python3 disagreeing with cmd",
            "differs from cmd's command",
            |r| edit_line(r, "python3.exe", |l| l.replace("--credential", "--other")),
        ),
        (
            "python disagreeing with python3",
            "same shim with the same arguments",
            |r| {
                edit_line(r, "python.exe", |l| {
                    l.replace("0123456789abcdef", "fedcba9876543210")
                })
            },
        ),
        (
            "conhost started with another flag",
            "conhost.exe 0x4",
            |r| edit_line(r, "conhost.exe", |l| l.replace(" 0x4", " 0x0")),
        ),
        ("conhost with an extra argument", "conhost.exe 0x4", |r| {
            edit_line(r, "conhost.exe", |l| format!("{l} --headless"))
        }),
        (
            "a conhost row whose argv[0] is another program",
            "conhost.exe 0x4",
            |r| {
                edit_line(r, "conhost.exe", |l| {
                    l.replace(r"\conhost.exe", r"\evil.exe")
                })
            },
        ),
        (
            "the real interpreter is not python",
            "not a childless python",
            |r| row_by_image(r, "python.exe").image = "pythonw.exe".into(),
        ),
        (
            "the real interpreter's argv[0] is not python",
            "argv[0] is not python",
            |r| {
                edit_line(r, "python.exe", |l| {
                    l.replacen(r"\python.exe", r"\pythonw.exe", 1)
                })
            },
        ),
        (
            "a second conhost in place of the python3 hop",
            "one conhost and one python3",
            |r| {
                // cmd -> { conhost 0x4, conhost 0x4 -> python }: two conhosts.
                let hop = row_by_image(r, "python3.exe");
                hop.image = "conhost.exe".into();
                hop.cmdline = Some(r"\??\C:\WINDOWS\system32\conhost.exe 0x4".into());
            },
        ),
        (
            "python beside the hop instead of under it",
            "exactly two children",
            |r| {
                // cmd -> { conhost, python3, python }: still four members.
                let cmd = row_by_image(r, "cmd.exe").pid;
                row_by_image(r, "python.exe").ppid = cmd;
            },
        ),
    ];
    let f = fixture("windows-shim-chain");
    for (what, _, edit) in &four {
        let mut rows = f.table.clone();
        edit(&mut rows);
        let members = subtree(&table_of(&rows), shim_pid(&f)).map(|m| m.len());
        assert_eq!(
            members,
            Some(4),
            "{what} must keep the subtree at four members"
        );
    }
    assert_each_refused_by_its_rule("windows-shim-chain", &four);
}

#[test]
fn a_cmd_running_anything_else_is_work_on_windows() {
    let v = mutate_chain("windows-shim-chain", |r| {
        row_by_image(r, "cmd.exe").cmdline = Some(r#"C:\WINDOWS\system32\cmd.exe /c dir"#.into());
    });
    assert_eq!(v.class, ChildClass::Work);
}

// --- the parsers --------------------------------------------------------------

#[test]
fn windows_args_follow_the_command_line_to_argv_rules() {
    let w = split_windows_args;
    assert_eq!(w(r#"a "b c" d"#), ["a", "b c", "d"]);
    assert_eq!(w(r#""C:\p q\x.exe"  "y""#), [r"C:\p q\x.exe", "y"]);
    assert_eq!(w(r#"a\\\"b"#), [r#"a\"b"#]);
    assert_eq!(w(r#""a\\" b"#), [r"a\", "b"]);
    assert_eq!(w(r#""a""b""#), [r#"a"b"#]);
    assert_eq!(w(r"C:\x\y.exe 0x4"), [r"C:\x\y.exe", "0x4"]);
    assert!(w("   ").is_empty());
}

#[test]
fn cmd_inner_command_strips_the_outer_quotes_and_carets() {
    assert_eq!(
        cmd_inner_command(r#"C:\WINDOWS\system32\cmd.exe /d /s /c "py ^"a b^" ^"c^"""#).as_deref(),
        Some(r#"py "a b" "c""#)
    );
    assert_eq!(
        cmd_inner_command(r#"cmd.exe /D /S /C "x""#).as_deref(),
        Some("x")
    );
    assert_eq!(
        cmd_inner_command(r#""C:\Program Files\cmd.exe" /d /s /c "x""#).as_deref(),
        Some("x")
    );
    assert_eq!(cmd_inner_command(r#"cmd.exe /d /c "x""#), None);
    assert_eq!(cmd_inner_command(r#"cmd.exe /d /s /c x"#), None);
    assert_eq!(cmd_inner_command(r#"cmd.exe /d /s /c "x^""#), None);
    assert_eq!(cmd_inner_command(r#"bash.exe /d /s /c "x""#), None);
    assert_eq!(cmd_inner_command("cmd.exe"), None);
    assert_eq!(cmd_inner_command(r#""cmd.exe"#), None);
}

#[test]
fn cmd_inner_command_refuses_multibyte_text_without_panicking() {
    // Each cut lands on, or right beside, a character wider than one byte: a
    // byte-offset slice here would panic on a char boundary.
    for line in [
        "cmd.exe /é /s /c \"x\"",
        "cmd.exe /dé /s /c \"x\"",
        "cmd.exe é",
        "\"C:\\é\\cmd.exe\" /d /s /c \"x\"",
        "\"C:\\é\\cmd.exe\" /d /s /c \"é^é\"",
        "\u{1F600}cmd.exe /d /s /c \"x\"",
        "cmd.exe /d /s /c \"x\u{1F600}",
        "cmd.exe\u{3000}/d /s /c \"x\"",
        "é",
        "\"é",
    ] {
        let _ = cmd_inner_command(line);
    }
    assert_eq!(
        cmd_inner_command("\"C:\\é\\cmd.exe\" /d /s /c \"é^é\"").as_deref(),
        Some("éé")
    );
    assert_eq!(cmd_inner_command("cmd.exe /dé /s /c \"x\""), None);
}

#[test]
fn python_names() {
    for yes in ["python", "python3", "python3.13", "python3.14"] {
        assert!(is_python(yes), "{yes}");
    }
    for no in [
        "python2",
        "python3.",
        "python3.x",
        "pythonw",
        "py",
        "ipython",
    ] {
        assert!(!is_python(no), "{no}");
    }
}

#[test]
fn a_linux_backslash_is_part_of_a_file_name_not_a_separator() {
    let v = linux_shim_with_argv(|a| {
        a[1] = "/home/x/qontinui-claude-config\\scripts/coord-mcp-shim.py".into();
    });
    assert_eq!(v.class, ChildClass::Work, "{v:?}");
}

#[test]
fn an_agent_worktrees_config_checkout_is_the_measured_location_too() {
    // The suffix pins the repo's `scripts/`, not which checkout of it.
    let v = linux_shim_with_argv(|a| {
        a[1] = "/home/x/agent-worktrees/0193-abcd/qontinui-claude-config/scripts/coord-mcp-shim.py"
            .into();
    });
    assert_eq!(v.class, ChildClass::ShimOrPump, "{v:?}");
}

#[test]
fn a_childless_claude_is_admitted_and_says_so() {
    let mut t = ProcTable::new(true);
    t.insert(
        10,
        ProcRow {
            ppid: 1,
            image: Some("claude".into()),
            start: Some(5),
        },
    );
    let v = classify_session(Platform::Linux, 10, &t, Some(&HashMap::new()), &t);
    assert!(v.admit);
    assert_eq!(v.classes(), "no children");
}

#[test]
fn a_cyclic_table_is_unknown_not_a_hang() {
    let mut t = ProcTable::new(true);
    t.insert(
        2,
        ProcRow {
            ppid: 3,
            image: Some("python3".into()),
            start: Some(1),
        },
    );
    t.insert(
        3,
        ProcRow {
            ppid: 2,
            image: Some("python3".into()),
            start: Some(1),
        },
    );
    let v = classify_child(Platform::Linux, 2, &t, Some(&HashMap::new()), &t);
    assert_eq!(v.class, ChildClass::Unknown);
}
