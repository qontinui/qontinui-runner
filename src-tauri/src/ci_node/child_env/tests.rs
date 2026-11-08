//! Tests for [`super`]: the allowlist as a pure function, a real child under
//! each policy, the call-site convention in `tools.rs`, and two `#[ignore]`d
//! end-to-end runs through the production `provision()`.
//!
//! Every expected name is spelled out HERE as a literal and never read back
//! from the constant under test — a test that compares a constant with itself
//! cannot fail (served policy `testing` `a-test-must-be-able-to-fail`).
//!
//! NAMES, NEVER VALUES. Every child these tests start prints the names of its
//! environment and nothing else, and every assertion and failure message
//! carries names only. A mutation run (no `env_clear()`) spawns children that
//! inherit the whole test environment, which on a developer box holds
//! plaintext passwords.

use super::*;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::manifest::CiTool;
use super::super::package_index::{self, PackageIndex};
use super::super::tools::{self, Installer, ToolSpec};

// ── Literals ─────────────────────────────────────────────────────────────

/// [`PROVISIONING_BASE_ENV`], spelled out.
fn base_names() -> Vec<&'static str> {
    let mut names = vec![
        "PATH",
        "HOME",
        "USERPROFILE",
        "TMPDIR",
        "TEMP",
        "TMP",
        "SystemRoot",
        "APPDATA",
        "LOCALAPPDATA",
        "ComSpec",
        "PATHEXT",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
    ];
    if cfg!(target_os = "linux") {
        names.push("LD_LIBRARY_PATH");
    }
    names
}

/// [`PROVISIONING_NETWORK_ENV`], spelled out.
const NETWORK_NAMES: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
];

/// The proxy canary. ci.yml's `Provisioning env e2e` step points it at a
/// closed loopback port (harmless under `--no-index`); it must reach no child
/// but `pip install`.
const CANARY_PROXY: &str = "HTTPS_PROXY";
/// The credential canary the same step sets. It must reach no child at all.
const CANARY_PASSWORD: &str = "QONTINUI_E2E_CANARY_PASSWORD";

/// Names that must never reach a provisioning child: the runner's named
/// credential class, the usual CI and cloud tokens, a hook into npm, the
/// redirects the old five-prefix denylist existed for, and the canary.
fn hostile_names() -> Vec<String> {
    let mut names: Vec<String> = [
        "QONTINUI_OPERATOR2_PASSWORD",
        "QONTINUI_TEST_LOGIN_PASSWORD",
        "QONTINUI_TEST_AUTO_LOGIN_PASSWORD",
        "GITHUB_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "NODE_OPTIONS",
        "PIP_INDEX_URL",
        "PYTHONPATH",
        "VIRTUAL_ENV",
        "npm_config_prefix",
        CANARY_PASSWORD,
    ]
    .iter()
    .map(|name| (*name).to_string())
    .collect();
    // ...and whatever the runner's named credential class grows to. This is
    // an INPUT to the test, not an expectation: it only adds hostile names.
    names.extend(
        crate::terminal::CREDENTIAL_VALUE_ENV_VARS
            .iter()
            .map(|name| (*name).to_string()),
    );
    names
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// A synthetic inherited environment; every value is derived from its name.
fn synthetic<S: AsRef<str>>(names: &[S]) -> Vec<(OsString, OsString)> {
    names
        .iter()
        .map(|name| {
            let name = name.as_ref();
            (
                OsString::from(name),
                OsString::from(format!("value-of-{name}")),
            )
        })
        .collect()
}

fn names_of(env: &[(OsString, OsString)]) -> Vec<String> {
    env.iter()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect()
}

fn sorted<S: AsRef<str>>(names: &[S]) -> Vec<String> {
    let mut out: Vec<String> = names.iter().map(|n| n.as_ref().to_string()).collect();
    out.sort();
    out
}

/// One environment name compared with another the way the OS compares them:
/// case-insensitively on Windows (where CPython also upper-cases
/// `os.environ`), exactly elsewhere.
fn same(a: &str, b: &str) -> bool {
    if cfg!(target_os = "windows") {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn holds<S: AsRef<str>>(set: &[S], name: &str) -> bool {
    set.iter().any(|listed| same(listed.as_ref(), name))
}

/// The first `file` on this process's PATH.
fn find_on_path(file: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(file))
        .find(|candidate| candidate.is_file())
}

/// The tokens a child reports, one per line. A printer means to print names,
/// but it cannot promise to: the Windows `for /f … set` printer echoes the
/// start of every continuation line of a multi-line VALUE. So a token is only
/// ever compared or counted here, and printed only through [`shown`].
fn reported_names(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Is `token` shaped like an environment NAME (`^[A-Za-z_][A-Za-z0-9_()]*$`,
/// the parentheses for Windows' `ProgramFiles(x86)`)? Only such a token is
/// ever printed; any other may be part of a value, so it counts as unexpected
/// and is never printed.
fn printable_name(token: &str) -> bool {
    let mut chars = token.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '(' | ')'))
}

/// `tokens` as a failure message may show them: the printable names, and a
/// COUNT of the rest — never their text.
fn shown<S: AsRef<str>>(tokens: &[S]) -> String {
    let printable: Vec<&str> = tokens
        .iter()
        .map(AsRef::as_ref)
        .filter(|token| printable_name(token))
        .collect();
    match tokens.len() - printable.len() {
        0 => format!("{printable:?}"),
        withheld => format!("{printable:?} and {withheld} unprintable token(s), withheld"),
    }
}

// ── The allowlist, as a pure function ───────────────────────────────────

/// The base policy keeps exactly the base names and adds the explicit pip
/// config, whatever else the host carries.
#[test]
fn a_provisioning_child_inherits_the_base_names_and_nothing_else() {
    let mut host: Vec<String> = base_names().iter().map(|n| (*n).to_string()).collect();
    host.extend(NETWORK_NAMES.iter().map(|n| (*n).to_string()));
    host.extend(hostile_names());
    // The host's OWN pip config, which must be replaced, never inherited.
    host.push("PIP_CONFIG_FILE".to_string());

    let env = provisioning_env(ChildEnv::Provisioning, synthetic(&host))
        .expect("a provisioning policy builds an environment");

    let mut want: Vec<&str> = base_names();
    want.push("PIP_CONFIG_FILE");
    assert_eq!(sorted(&names_of(&env)), sorted(&want));

    // The pip config is the null device in pip's own spelling, ONCE: the
    // host's value is gone rather than shadowed.
    let null = if cfg!(target_os = "windows") {
        "nul"
    } else {
        "/dev/null"
    };
    let pip: Vec<&OsString> = env
        .iter()
        .filter(|(name, _)| name == "PIP_CONFIG_FILE")
        .map(|(_, value)| value)
        .collect();
    assert_eq!(pip, vec![&OsString::from(null)]);

    // Every inherited value passes through unchanged.
    for (name, value) in &env {
        if name != "PIP_CONFIG_FILE" {
            let name = name.to_string_lossy();
            assert_eq!(
                value,
                &OsString::from(format!("value-of-{name}")),
                "{name} was altered"
            );
        }
    }
}

/// Only `pip install`'s policy carries the proxy and CA names (D2).
#[test]
fn only_the_network_policy_inherits_the_proxy_and_ca_names() {
    let mut host: Vec<String> = base_names().iter().map(|n| (*n).to_string()).collect();
    host.extend(NETWORK_NAMES.iter().map(|n| (*n).to_string()));
    host.extend(hostile_names());

    let env = provisioning_env(ChildEnv::ProvisioningNetwork, synthetic(&host))
        .expect("a provisioning policy builds an environment");
    let mut want: Vec<&str> = base_names();
    want.extend(NETWORK_NAMES.iter().copied());
    want.push("PIP_CONFIG_FILE");
    assert_eq!(sorted(&names_of(&env)), sorted(&want));

    // The base policy refuses every one of them.
    let env = provisioning_env(ChildEnv::Provisioning, synthetic(NETWORK_NAMES))
        .expect("a provisioning policy builds an environment");
    assert_eq!(names_of(&env), vec!["PIP_CONFIG_FILE"]);
}

/// The container policy changes nothing: it inherits, so it builds no
/// environment and admits every name.
#[test]
fn the_container_policy_inherits_everything() {
    assert!(provisioning_env(ChildEnv::ContainerRuntime, synthetic(&hostile_names())).is_none());
    for name in hostile_names() {
        assert!(admits(ChildEnv::ContainerRuntime, OsStr::new(&name)));
    }
}

/// `Path`, `SYSTEMROOT` and friends are the same variables as the listed
/// spellings on Windows, and different variables everywhere else.
#[test]
fn other_spellings_match_case_insensitively_on_windows_only() {
    let host = ["Path", "SYSTEMROOT", "comspec", "Https_Proxy"];
    let base = provisioning_env(ChildEnv::Provisioning, synthetic(&host))
        .expect("a provisioning policy builds an environment");
    let network = provisioning_env(ChildEnv::ProvisioningNetwork, synthetic(&host))
        .expect("a provisioning policy builds an environment");
    if cfg!(target_os = "windows") {
        // The ORIGINAL spelling is kept: Windows treats them as one name.
        assert_eq!(
            names_of(&base),
            vec!["Path", "SYSTEMROOT", "comspec", "PIP_CONFIG_FILE"]
        );
        assert_eq!(
            names_of(&network),
            vec![
                "Path",
                "SYSTEMROOT",
                "comspec",
                "Https_Proxy",
                "PIP_CONFIG_FILE"
            ]
        );
    } else {
        assert_eq!(names_of(&base), vec!["PIP_CONFIG_FILE"]);
        assert_eq!(names_of(&network), vec!["PIP_CONFIG_FILE"]);
    }
}

/// What the spawn helper sets explicitly (`tokio_no_window`'s git posture) is
/// judged by the same allowlist, and so is a host's `PIP_CONFIG_FILE`.
#[test]
fn a_name_outside_the_allowlist_is_refused_whoever_set_it() {
    for policy in [ChildEnv::Provisioning, ChildEnv::ProvisioningNetwork] {
        for refused in [
            "GIT_TERMINAL_PROMPT",
            "GIT_ASKPASS",
            "GCM_INTERACTIVE",
            "PIP_CONFIG_FILE",
            CANARY_PASSWORD,
        ] {
            assert!(
                !admits(policy, OsStr::new(refused)),
                "{policy:?} admits {refused}"
            );
        }
        assert!(
            admits(policy, OsStr::new("PATH")),
            "{policy:?} refuses PATH"
        );
    }
    assert_eq!(
        admits(ChildEnv::Provisioning, OsStr::new("LD_LIBRARY_PATH")),
        cfg!(target_os = "linux"),
        "LD_LIBRARY_PATH is a Linux-only entry"
    );
}

// ── A real child ─────────────────────────────────────────────────────────

/// The explicit `envs` name the spawn test hands its child.
const EXPLICIT: &str = "QONTINUI_CHILD_ENV_EXPLICIT";

/// Names the name-printing child's own interpreter adds on top of what
/// `build` passes. Measured, and each one commented with its source.
fn printer_allowance() -> Vec<&'static str> {
    if cfg!(target_os = "windows") {
        // cmd.exe, which runs the printer script, may define PROMPT for the
        // batch it runs. UNMEASURED here — the windows `test` leg is the
        // measurement.
        vec!["PROMPT"]
    } else {
        // gawk puts AWKPATH and AWKLIBPATH into ENVIRON (its manual). mawk adds
        // nothing: measured 2026-11-07 with mawk 1.3.4, `env -i
        // PATH=/usr/bin:/bin awk 'BEGIN { for (k in ENVIRON) print k }'`
        // printed `PATH` alone.
        vec!["AWKPATH", "AWKLIBPATH"]
    }
}

/// Start a child under `policy` that prints the NAMES of its environment.
///
/// Unix: `awk` over `ENVIRON`, which lists keys only — `env | cut -d= -f1`
/// would print a line of a multi-line value. Windows: a `for /f` over `set`
/// that echoes only the text before the first `=`.
async fn names_seen_by_a_child(policy: ChildEnv, envs: &[(String, String)]) -> Vec<String> {
    #[cfg(target_os = "windows")]
    let (_script_dir, program, args) = {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("print-env-names.cmd");
        std::fs::write(
            &script,
            "@echo off\r\nfor /f \"delims==\" %%v in ('set') do @echo %%v\r\n",
        )
        .expect("write the name printer");
        let script = script.to_string_lossy().into_owned();
        (
            dir,
            "cmd.exe",
            vec!["/D".to_string(), "/C".to_string(), script],
        )
    };
    #[cfg(not(target_os = "windows"))]
    let (program, args) = (
        "awk",
        vec!["BEGIN { for (k in ENVIRON) print k }".to_string()],
    );
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = tools::run_capture_env(program, &args, Duration::from_secs(60), None, policy, envs)
        .await
        .unwrap_or_else(|e| {
            panic!(
                "the name-printing child did not run: {}",
                printer_failure(&e)
            )
        });
    reported_names(&out)
}

/// A name printer's failure, fit for a panic message. `run_capture_env`
/// appends the child's OUTPUT to an exit failure (`… exited <status>
/// (<output>)`), and a printer's output may hold a value fragment, so only the
/// command and status are kept. A spawn or timeout failure carries no output.
fn printer_failure(error: &str) -> String {
    match error.split_once(" exited ") {
        Some((command, rest)) => {
            let status = rest.split(" (").next().unwrap_or_default();
            format!("{command} exited {status} (its output is withheld)")
        }
        None => error.to_string(),
    }
}

/// A provisioning child sees the allowlist, the explicit pip config and its
/// caller's explicit `envs` — and nothing else of this test's environment.
#[tokio::test]
async fn a_provisioning_child_sees_only_allowlisted_names_and_its_explicit_env() {
    let mut expected: Vec<&str> = base_names();
    expected.extend(["PIP_CONFIG_FILE", EXPLICIT]);
    expected.extend(printer_allowance());

    // Without a name the allowlist refuses in THIS process's environment, the
    // child could not show a missing `env_clear()`, and the test would prove
    // nothing. Every real test process has one (`USER`, `RUNNER_*`,
    // `CARGO_*`, `COMPUTERNAME`, ...); refuse to pass vacuously if not.
    let foreign: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .filter(|name| !holds(&expected, name))
        .collect();
    assert!(
        !foreign.is_empty(),
        "this test's own environment carries only allowlisted names, so a child \
         that inherited it would look identical to one that did not"
    );

    let seen = names_seen_by_a_child(
        ChildEnv::Provisioning,
        &[(EXPLICIT.to_string(), "1".to_string())],
    )
    .await;

    // A token that is not name-shaped is unexpected whatever it says.
    let unexpected: Vec<&String> = seen
        .iter()
        .filter(|token| !printable_name(token) || !holds(&expected, token))
        .collect();
    assert!(
        unexpected.is_empty(),
        "a provisioning child saw names outside the allowlist: {}",
        shown(&unexpected)
    );
    for required in ["PATH", "PIP_CONFIG_FILE", EXPLICIT] {
        assert!(
            holds(&seen, required),
            "{required} did not reach the child; it saw {}",
            shown(&seen)
        );
    }
}

/// `tokio_no_window` sets the prompt-proof git posture EXPLICITLY on any
/// program named `git`, and `env_clear()` would drop it with everything else —
/// so `build` reads those back and re-applies only what the allowlist admits.
/// None of the three is admitted, so none may arrive.
///
/// Unix only: the program has to be NAMED git to get the posture, and this
/// borrows awk under that name through a symlink. The rule itself is
/// platform-independent and pinned by the pure test above.
#[cfg(unix)]
#[tokio::test]
async fn what_the_spawn_helper_sets_reaches_the_child_only_through_the_allowlist() {
    const HELPER_SET: [&str; 3] = ["GIT_TERMINAL_PROMPT", "GIT_ASKPASS", "GCM_INTERACTIVE"];
    let awk = find_on_path("awk").expect("awk on PATH");
    let dir = tempfile::tempdir().expect("tempdir");
    let git = dir.path().join("git");
    std::os::unix::fs::symlink(&awk, &git).expect("symlink awk as git");
    let git = git.to_string_lossy().into_owned();
    let printer = ["BEGIN { for (k in ENVIRON) print k }"];

    // Precondition: under a policy that inherits, the helper's names DO arrive,
    // so their absence below is the allowlist's doing and not the helper's.
    let inherited = tools::run_capture_env(
        &git,
        &printer,
        Duration::from_secs(60),
        None,
        ChildEnv::ContainerRuntime,
        &[],
    )
    .await
    .unwrap_or_else(|e| panic!("the git-named printer did not run: {}", printer_failure(&e)));
    let inherited = reported_names(&inherited);
    for name in HELPER_SET {
        assert!(
            holds(&inherited, name),
            "tokio_no_window no longer sets {name} on a git program, so this \
             test no longer exercises the re-apply path"
        );
    }

    let out = tools::run_capture_env(
        &git,
        &printer,
        Duration::from_secs(60),
        None,
        ChildEnv::Provisioning,
        &[],
    )
    .await
    .unwrap_or_else(|e| panic!("the git-named printer did not run: {}", printer_failure(&e)));
    let seen = reported_names(&out);
    for name in HELPER_SET {
        assert!(
            !holds(&seen, name),
            "{name}, set explicitly by the spawn helper, reached a provisioning \
             child although the allowlist refuses it"
        );
    }
    assert!(holds(&seen, "PATH"), "PATH did not reach the child");
}

// ── The call-site convention in tools.rs ────────────────────────────────

/// `tools.rs` names only the two provisioning policies, and hands the network
/// one to `pip install` alone (D2). The container policy is `services.rs`'s.
///
/// Read from the source, outside its `mod tests` and with comment-only lines
/// dropped, because the property is about which CALL gets which policy — a
/// type cannot say that.
#[test]
fn tools_rs_hands_the_network_policy_to_pip_install_alone() {
    const SOURCE: &str = include_str!("../tools.rs");
    const TESTS_MARKER: &str = "#[cfg(test)]\nmod tests {";
    let production = SOURCE
        .split_once(TESTS_MARKER)
        .map(|(before, _)| before)
        .unwrap_or_else(|| panic!("tools.rs no longer contains `{TESTS_MARKER}`"));
    let code: String = production
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    assert_eq!(
        code.matches("ContainerRuntime").count(),
        0,
        "tools.rs must not name the container policy: its provisioning children \
         take `Provisioning`, or `ProvisioningNetwork` for `pip install`"
    );

    let network: Vec<usize> = code
        .match_indices("ProvisioningNetwork")
        .map(|(at, _)| at)
        .collect();
    assert_eq!(
        network.len(),
        1,
        "tools.rs must hand `ProvisioningNetwork` to exactly one call, `pip \
         install`; any other provisioning child would see the proxy names"
    );
    let before = code
        .get(..network[0])
        .expect("a match index is a char boundary");
    let (_, function) = before
        .rsplit_once("fn ")
        .expect("the network policy sits inside a function");
    assert!(
        function.starts_with("install_python_package("),
        "the network policy must be inside `install_python_package`"
    );
    let (_, call) = before
        .rsplit_once("run_capture(")
        .expect("the network policy is an argument to a `run_capture` call");
    assert!(
        call.contains("pip"),
        "the call handed the network policy must be the `pip install` one, \
         got: run_capture({call}"
    );
}

// ── End to end through `provision()` ─────────────────────────────────────

const PROBE_DISTRIBUTION: &str = "qontinui-env-probe";
const PROBE_MODULE: &str = "qontinui_env_probe";
const PROBE_VERSION: &str = "1.0.0";
/// Where the probe records the names it saw: beside its venv, `sys.prefix`.
const SEEN_FILE: &str = "seen-env-names.txt";

/// The probe's `python -m` entry. It writes the NAMES of its environment,
/// sorted, to `<sys.prefix>/seen-env-names.txt` — never a value — and then
/// reports its version, so `verify` accepts it.
const PROBE_MAIN: &str = r#"import os
import sys

if sys.argv[1:] != ["--version"]:
    sys.stderr.write("qontinui-env-probe: only --version is supported\n")
    sys.exit(2)
with open(os.path.join(sys.prefix, "seen-env-names.txt"), "w", encoding="utf-8") as seen:
    for name in sorted(os.environ):
        seen.write(name + "\n")
print("qontinui-env-probe 1.0.0")
"#;

/// Build `qontinui_env_probe-1.0.0-py3-none-any.whl`, a pure-Python wheel with
/// a correct RECORD, into `dir`.
fn write_probe_wheel(dir: &Path) {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    use std::io::Write as _;

    let dist_info = format!("{PROBE_MODULE}-{PROBE_VERSION}.dist-info");
    let mut files: Vec<(String, String)> = vec![
        (format!("{PROBE_MODULE}/__init__.py"), String::new()),
        (format!("{PROBE_MODULE}/__main__.py"), PROBE_MAIN.to_string()),
        (
            format!("{dist_info}/METADATA"),
            format!("Metadata-Version: 2.1\nName: {PROBE_DISTRIBUTION}\nVersion: {PROBE_VERSION}\n"),
        ),
        (
            format!("{dist_info}/WHEEL"),
            "Wheel-Version: 1.0\nGenerator: qontinui-runner-tests\nRoot-Is-Purelib: true\nTag: py3-none-any\n"
                .to_string(),
        ),
    ];
    let mut record = String::new();
    for (path, body) in &files {
        let digest = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(body.as_bytes()));
        record.push_str(&format!("{path},sha256={digest},{}\n", body.len()));
    }
    record.push_str(&format!("{dist_info}/RECORD,,\n"));
    files.push((format!("{dist_info}/RECORD"), record));

    std::fs::create_dir_all(dir).expect("create the wheel directory");
    let wheel = dir.join(format!("{PROBE_MODULE}-{PROBE_VERSION}-py3-none-any.whl"));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&wheel).expect("create the wheel"));
    for (path, body) in &files {
        zip.start_file(path.as_str(), zip::write::SimpleFileOptions::default())
            .expect("start a wheel entry");
        zip.write_all(body.as_bytes()).expect("write a wheel entry");
    }
    zip.finish().expect("finish the wheel");
}

/// Names the probe's own interpreters add on top of what `build` passes.
/// Measured where reachable, and each one commented with its source.
fn probe_allowance() -> Vec<&'static str> {
    if cfg!(target_os = "windows") {
        vec![
            // cmd.exe, which runs the `.cmd` shim, may define PROMPT.
            // UNMEASURED here — the windows `test` leg is the measurement.
            "PROMPT",
            // A venv's Scripts\python.exe is a launcher that hands the venv to
            // the base interpreter through this variable. UNMEASURED here.
            "__PYVENV_LAUNCHER__",
        ]
    } else {
        // dash, the `#!/bin/sh` that runs the Python shim on Debian and
        // Ubuntu, exports PWD. Measured 2026-11-07: `env -i PATH=/usr/bin:/bin
        // /bin/sh -c 'exec python3 -I -c "import os; print(sorted(os.environ))"'`
        // printed ['LC_CTYPE', 'PATH', 'PWD']. LC_CTYPE there is CPython's own
        // PEP 538 locale coercion (no locale was set); it is in the base set.
        vec!["PWD"]
    }
}

/// cmd.exe keeps per-drive current directories and exit codes in hidden
/// variables — `=C:` for each drive, `=ExitCode`, `=ExitCodeAscii` — and the
/// CRT normally hides them from `os.environ`. They are cmd.exe's own records,
/// never the host's, and only these exact shapes are allowed.
fn hidden_cmd_variable(name: &str) -> bool {
    cfg!(target_os = "windows")
        && match name.strip_prefix('=') {
            Some("ExitCode" | "ExitCodeAscii") => true,
            Some(drive) => {
                let mut chars = drive.chars();
                matches!(
                    (chars.next(), chars.next(), chars.next()),
                    (Some(letter), Some(':'), None) if letter.is_ascii_alphabetic()
                )
            }
            None => false,
        }
}

/// Both bounds on what one provisioning child (the probe) recorded.
fn assert_probe_saw_only_allowlisted_names(pass: &str, seen: &[String]) {
    // The canaries first, so the regression they exist for names itself.
    assert!(
        !holds(seen, CANARY_PASSWORD),
        "{pass}: {CANARY_PASSWORD} reached a provisioning child — its \
         environment was not cleared"
    );
    assert!(
        !holds(seen, CANARY_PROXY),
        "{pass}: {CANARY_PROXY} reached the verify child — only `pip install` \
         may see the network names"
    );
    // Lower bound: the child came through `build`.
    for required in ["PATH", "PIP_CONFIG_FILE"] {
        assert!(
            holds(seen, required),
            "{pass}: {required} is missing, so the probe did not come through \
             `build`; it saw {}",
            shown(seen)
        );
    }
    // Upper bound.
    let mut allowed: Vec<&str> = base_names();
    allowed.push("PIP_CONFIG_FILE");
    allowed.extend(probe_allowance());
    // A token that is not name-shaped is unexpected whatever it says.
    let unexpected: Vec<&String> = seen
        .iter()
        .filter(|token| {
            !hidden_cmd_variable(token) && (!printable_name(token) || !holds(&allowed, token))
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "{pass}: names outside the allowlist reached a provisioning child: {}",
        shown(&unexpected)
    );
}

/// The proof, on whichever host runs it: a pure-Python probe provisioned
/// through `provision()` — the function `executor::run_dispatch` calls —
/// records the names its `verify` child saw, on a cache miss and again on a
/// cache hit; then the host's node tree (npm and npx companions) verifies
/// under the same environment.
///
/// `#[ignore]`d because it needs a host Python 3 with `venv` and a host node,
/// which a suite test must not assume (plan D5). ci.yml's `Provisioning env
/// e2e` step runs it on both `test` legs with the two canaries set.
#[tokio::test]
#[ignore = "needs a host Python 3 with venv and a host node; ci.yml `Provisioning env e2e` runs it with its canaries set"]
async fn provisioning_children_see_only_allowlisted_names() {
    // A canary that is not in this process's environment cannot be missing
    // from a child's, so its absence below would prove nothing.
    for canary in [CANARY_PROXY, CANARY_PASSWORD] {
        assert!(
            std::env::var_os(canary).is_some(),
            "{canary} is not set in this test's environment, so the test would \
             prove nothing. Set {CANARY_PROXY}=http://127.0.0.1:9 and \
             {CANARY_PASSWORD}=canary, as ci.yml's `Provisioning env e2e` does"
        );
    }

    let scratch = tempfile::tempdir().expect("tempdir");
    let wheels = scratch.path().join("wheels");
    write_probe_wheel(&wheels);
    let wheels: &'static Path = Box::leak(wheels.into_boxed_path());
    package_index::register_test_tool(ToolSpec {
        name: PROBE_DISTRIBUTION,
        verify_args: &["--version"],
        install: Installer::PythonPackage {
            distribution: PROBE_DISTRIBUTION,
            module: PROBE_MODULE,
            command: PROBE_DISTRIBUTION,
            index: PackageIndex::LocalWheels(wheels),
        },
    });

    let root = scratch.path().join("root");
    let declared = [CiTool {
        name: PROBE_DISTRIBUTION.to_string(),
        version: PROBE_VERSION.to_string(),
    }];
    // The first call verifies in staging and the file moves into place with
    // the rename; the second is the cache-hit `verify_installed`. The file is
    // deleted before EACH call, so a cache hit that stopped running the probe
    // cannot pass on the first call's file.
    let seen_file = tools::tool_dir(&root, PROBE_DISTRIBUTION, PROBE_VERSION)
        .join("venv")
        .join(SEEN_FILE);
    for (pass, path_marker) in [("install", "installed at"), ("cache hit", "cache hit")] {
        match std::fs::remove_file(&seen_file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("{pass}: could not delete {}: {e}", seen_file.display()),
        }
        let mut log: Vec<String> = Vec::new();
        let outcome = tools::provision(&root, &declared, &mut |line| log.push(line)).await;
        if let Err(e) = outcome {
            panic!("{pass}: provisioning the probe failed: {e}\nlog: {log:#?}");
        }
        assert!(
            log.iter().any(|line| line.contains(path_marker)),
            "{pass}: no `{path_marker}` line, so this call did not take the path \
             it is named for; log: {log:#?}"
        );
        let seen = std::fs::read_to_string(&seen_file).unwrap_or_else(|e| {
            panic!(
                "{pass}: the probe did not run — {} was not re-created: {e}",
                seen_file.display()
            )
        });
        assert_probe_saw_only_allowlisted_names(pass, &reported_names(&seen));
    }

    // The node arm: the registry's node entry verified (npm and npx
    // companions) against the host's node tree, under the same environment.
    // `resolve()` refuses an unreviewed node version, so it goes through the
    // test door that builds the node `Resolved` itself.
    let exe = if cfg!(target_os = "windows") {
        "node.exe"
    } else {
        "node"
    };
    let node = find_on_path(exe).expect("a host node on PATH");
    let bin_dir = node.parent().expect("node sits in a directory");
    let reported = tools::run_capture(
        &node.to_string_lossy(),
        &["--version"],
        Duration::from_secs(60),
        None,
        ChildEnv::Provisioning,
    )
    .await
    .unwrap_or_else(|e| panic!("the host node did not report a version: {e}"));
    let version = reported.trim().trim_start_matches('v').to_string();
    let provides = tools::verify_node_tree_for_test(bin_dir, &version)
        .await
        .unwrap_or_else(|e| panic!("the host node tree failed verification: {e}"));
    for companion in ["npm ", "npx "] {
        assert!(
            provides.iter().any(|line| line.starts_with(companion)),
            "no `{companion}` report from the node verification: {provides:?}"
        );
    }
}

/// A real, networked `provision()` of every registry entry under the new
/// environment: `pip install` fetches through it, and each tool's own
/// `--version` code runs under it. A measurement, run by hand once per
/// reachable OS — no CI step runs it (plan D5, 2.7). Run with `--nocapture`
/// to see each tool's outcome and reported versions.
#[tokio::test]
#[ignore = "networked: downloads every registry tool (GitHub, nodejs.org, PyPI); a once-per-OS measurement, not a gate"]
async fn networked_provision_of_every_registry_tool() {
    // The registry's own measured pins (see each entry in `tools.rs`).
    let pins = [
        ("cargo-nextest", "0.9.98"),
        ("node", "22.11.0"),
        ("poetry", "2.1.3"),
        ("twine", "6.1.0"),
        ("datamodel-code-generator", "0.28.5"),
    ];
    assert_eq!(
        pins.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        tools::known_tool_names(),
        "pin a version here for every registry entry"
    );
    let root = tempfile::tempdir().expect("tempdir");
    let mut failures = Vec::new();
    for (name, version) in pins {
        let declared = [CiTool {
            name: name.to_string(),
            version: version.to_string(),
        }];
        let mut log: Vec<String> = Vec::new();
        let started = std::time::Instant::now();
        let outcome = tools::provision(root.path(), &declared, &mut |line| log.push(line)).await;
        println!(
            "== {name} {version}: {} in {:.1}s",
            if outcome.is_ok() {
                "PROVISIONED"
            } else {
                "FAILED"
            },
            started.elapsed().as_secs_f64()
        );
        for line in &log {
            println!("   {line}");
        }
        if let Err(e) = outcome {
            println!("   error: {e}");
            failures.push(format!("{name} {version}: {e}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
