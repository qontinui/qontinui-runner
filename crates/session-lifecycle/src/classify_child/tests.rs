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
    cmdline: Option<String>,
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

fn argv_of(rows: &[FixtureRow]) -> HashMap<u32, String> {
    rows.iter()
        .filter_map(|r| r.cmdline.clone().map(|c| (r.pid, c)))
        .collect()
}

/// The fixture classified with one table serving as both reads.
fn run(name: &str) -> SessionVerdict {
    let f = fixture(name);
    let t = table_of(&f.table);
    classify_session(
        platform_of(&f),
        f.claude_pid,
        &t,
        Some(&argv_of(&f.table)),
        &t,
    )
}

fn shim_pid(f: &Fixture) -> u32 {
    let kids: Vec<&FixtureRow> = f.table.iter().filter(|r| r.ppid == f.claude_pid).collect();
    kids.iter()
        .find(|r| {
            let i = r.image.to_ascii_lowercase();
            i.starts_with("python") || i == "cmd.exe"
        })
        .map(|r| r.pid)
        .expect("fixture has a shim child")
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
        let v = classify_session(wrong, f.claude_pid, &t, Some(&argv_of(&f.table)), &t);
        assert!(!v.admit, "{name} admitted on {wrong:?}: {v:?}");
        let v = classify_session(
            Platform::Other,
            f.claude_pid,
            &t,
            Some(&argv_of(&f.table)),
            &t,
        );
        assert!(!v.admit, "{name} admitted on Other");
    }
}

// --- binding: pid + creation time, re-confirmed --------------------------------

fn mutate_fresh(name: &str, edit: impl FnOnce(&mut Vec<FixtureRow>, u32)) -> ChildVerdict {
    let f = fixture(name);
    let shim = shim_pid(&f);
    let first = table_of(&f.table);
    let mut rows = f.table.clone();
    edit(&mut rows, shim);
    let fresh = table_of(&rows);
    classify_child(
        platform_of(&f),
        shim,
        &first,
        Some(&argv_of(&f.table)),
        &fresh,
    )
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
        });
    });
    assert_eq!(v.class, ChildClass::Unknown);
    assert!(v.reason.contains("membership"), "{v:?}");
}

#[test]
fn a_member_whose_image_changed_between_the_reads_is_unknown() {
    // A pid reused inside one second (Linux starts are second-granular) by a
    // different program keeps its start and parent; only the image tells.
    let v = mutate_fresh("linux-shim", |rows, shim| {
        rows.iter_mut().find(|r| r.pid == shim).unwrap().image = "node".into();
    });
    assert_eq!(v.class, ChildClass::Unknown);
    assert!(v.reason.contains("image"), "{v:?}");
}

#[test]
fn a_shim_script_outside_the_fleet_config_repo_is_not_the_measured_chain() {
    let v = mutate_chain("linux-shim", |r| {
        let row = row_by_image(r, "python3");
        row.cmdline = Some(
            row.cmdline
                .clone()
                .unwrap()
                .replace("/qontinui-claude-config/scripts/", "/tmp/scripts/"),
        );
    });
    assert_eq!(v.class, ChildClass::Work);
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
        let v = classify_child(Platform::Linux, shim, &t, Some(&argv_of(&f.table)), &t);
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
    assert_eq!(
        classify_child(Platform::Linux, shim, &t, Some(&argv_of(&f.table)), &fresh).class,
        ChildClass::Unknown
    );
    assert_eq!(
        classify_child(Platform::Linux, shim, &fresh, Some(&argv_of(&f.table)), &t).class,
        ChildClass::Unknown
    );
    assert!(
        !classify_session(
            Platform::Linux,
            f.claude_pid,
            &t,
            Some(&argv_of(&f.table)),
            &fresh
        )
        .admit
    );
}

#[test]
fn a_reused_claude_refuses_the_session() {
    let f = fixture("linux-shim");
    let first = table_of(&f.table);
    let mut rows = f.table.clone();
    rows[0].start += 1;
    let fresh = table_of(&rows);
    let v = classify_session(
        Platform::Linux,
        f.claude_pid,
        &first,
        Some(&argv_of(&f.table)),
        &fresh,
    );
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
    });
    let fresh = table_of(&rows);
    let v = classify_session(
        Platform::Linux,
        f.claude_pid,
        &first,
        Some(&argv_of(&f.table)),
        &fresh,
    );
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
    // Windows: a pid absent from a `Some` command-line map is UNKNOWN.
    for name in ["linux-shim", "windows-shim-chain"] {
        let f = fixture(name);
        let t = table_of(&f.table);
        let mut argv = argv_of(&f.table);
        let deepest = f.table.last().unwrap().pid;
        argv.remove(&deepest);
        let v = classify_child(platform_of(&f), shim_pid(&f), &t, Some(&argv), &t);
        assert_eq!(v.class, ChildClass::Unknown, "{name}: {v:?}");
    }
}

#[test]
fn an_unreadable_image_is_unknown() {
    let f = fixture("windows-shim-chain");
    let mut t = table_of(&f.table);
    let deepest = f.table.last().unwrap().pid;
    let row = t.row(deepest).unwrap().clone();
    t.insert(deepest, ProcRow { image: None, ..row });
    let v = classify_child(
        Platform::Windows,
        shim_pid(&f),
        &t,
        Some(&argv_of(&f.table)),
        &t,
    );
    assert_eq!(v.class, ChildClass::Unknown);
}

// --- the chain must match WHOLE ------------------------------------------------

fn mutate_chain(name: &str, edit: impl FnOnce(&mut Vec<FixtureRow>)) -> ChildVerdict {
    let f = fixture(name);
    let shim = shim_pid(&f);
    let mut rows = f.table.clone();
    edit(&mut rows);
    let t = table_of(&rows);
    classify_child(platform_of(&f), shim, &t, Some(&argv_of(&rows)), &t)
}

fn row_by_image<'a>(rows: &'a mut [FixtureRow], image: &str) -> &'a mut FixtureRow {
    rows.iter_mut().find(|r| r.image == image).unwrap()
}

#[test]
fn the_measured_chains_are_each_mutation_partnered() {
    // Every rule the matchers apply, each broken alone, must stop the admit.
    type Edit = fn(&mut Vec<FixtureRow>);
    let linux: [(&str, Edit); 7] = [
        ("another script", |r| {
            row_by_image(r, "python3").cmdline = Some("/usr/bin/python3 /tmp/other.py".into())
        }),
        ("-c instead of a script", |r| {
            row_by_image(r, "python3").cmdline =
                Some("/usr/bin/python3 -c import os; /x/qontinui-claude-config/scripts/coord-mcp-shim.py".into())
        }),
        ("an extra flag", |r| {
            let row = row_by_image(r, "python3");
            row.cmdline = Some(format!("{} --verbose", row.cmdline.clone().unwrap()));
        }),
        ("a non-python interpreter", |r| {
            let row = row_by_image(r, "python3");
            row.cmdline = Some(row.cmdline.clone().unwrap().replacen(
                "/usr/bin/python3",
                "/usr/bin/node",
                1,
            ));
        }),
        ("a non-python image", |r| {
            row_by_image(r, "python3").image = "node".into()
        }),
        ("a script that only resembles the shim", |r| {
            let row = row_by_image(r, "python3");
            row.cmdline = Some(
                row.cmdline
                    .clone()
                    .unwrap()
                    .replace("coord-mcp-shim.py", "coord-mcp-shim.py.bak"),
            );
        }),
        ("a grandchild under the shim", |r| {
            let shim = row_by_image(r, "python3").pid;
            r.push(FixtureRow {
                pid: 77,
                ppid: shim,
                image: "sleep".into(),
                start: 9,
                cmdline: Some("sleep 10".into()),
            });
        }),
    ];
    for (what, edit) in linux {
        let v = mutate_chain("linux-shim", edit);
        assert_ne!(
            v.class,
            ChildClass::ShimOrPump,
            "linux {what} still admitted: {v:?}"
        );
    }

    let windows: [(&str, Edit); 9] = [
        ("cmd without /s", |r| {
            let row = row_by_image(r, "cmd.exe");
            row.cmdline = Some(row.cmdline.clone().unwrap().replacen(" /s ", " ", 1));
        }),
        ("cmd /k instead of /c", |r| {
            let row = row_by_image(r, "cmd.exe");
            row.cmdline = Some(row.cmdline.clone().unwrap().replacen(" /c ", " /k ", 1));
        }),
        ("cmd running another script", |r| {
            let row = row_by_image(r, "cmd.exe");
            row.cmdline = Some(
                row.cmdline
                    .clone()
                    .unwrap()
                    .replace("coord-mcp-shim.py", "evil.py"),
            );
        }),
        ("python3 disagreeing with cmd", |r| {
            let row = row_by_image(r, "python3.exe");
            row.cmdline = Some(
                row.cmdline
                    .clone()
                    .unwrap()
                    .replace("--credential", "--other"),
            );
        }),
        ("python disagreeing with python3", |r| {
            let row = row_by_image(r, "python.exe");
            row.cmdline = Some(
                row.cmdline
                    .clone()
                    .unwrap()
                    .replace("0123456789abcdef", "fedcba9876543210"),
            );
        }),
        ("no conhost", |r| r.retain(|x| x.image != "conhost.exe")),
        ("no python3 hop", |r| {
            // cmd -> python directly (the hop removed, python reparented).
            let cmd = row_by_image(r, "cmd.exe").pid;
            r.retain(|x| x.image != "python3.exe");
            row_by_image(r, "python.exe").ppid = cmd;
        }),
        ("a second conhost", |r| {
            let cmd = row_by_image(r, "cmd.exe").pid;
            let mut extra = row_by_image(r, "conhost.exe").clone();
            extra.pid = 55;
            extra.ppid = cmd;
            r.push(extra);
        }),
        ("a child under the real interpreter", |r| {
            let py = row_by_image(r, "python.exe").pid;
            r.push(FixtureRow {
                pid: 66,
                ppid: py,
                image: "git.exe".into(),
                start: 3,
                cmdline: Some("git status".into()),
            });
        }),
    ];
    for (what, edit) in windows {
        let v = mutate_chain("windows-shim-chain", edit);
        assert_ne!(
            v.class,
            ChildClass::ShimOrPump,
            "windows {what} still admitted: {v:?}"
        );
    }
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
    let w = |s: &str| split_args(Platform::Windows, s);
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
    assert_eq!(cmd_inner_command(r#"cmd.exe /d /c "x""#), None);
    assert_eq!(cmd_inner_command(r#"cmd.exe /d /s /c x"#), None);
    assert_eq!(cmd_inner_command(r#"cmd.exe /d /s /c "x^""#), None);
    assert_eq!(cmd_inner_command(r#"bash.exe /d /s /c "x""#), None);
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
