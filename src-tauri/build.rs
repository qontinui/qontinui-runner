fn main() {
    // Tell Cargo to re-run this build script (and re-embed the frontend) when
    // the dist directory changes.  Without this, incremental builds silently
    // serve a stale frontend bundle from the compile-time cache.
    println!("cargo:rerun-if-changed=../dist");

    // Embed the current git SHA so the running binary can report exactly which
    // commit it was built from. Surfaced via the runner /health endpoint and
    // the supervisor's spawn-test response — lets manual-test sessions assert
    // "this temp runner is the commit I'm debugging" without guessing from
    // binary mtime.
    let git_sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok().map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=QONTINUI_GIT_SHA={}", git_sha);
    // Re-run this script when HEAD moves. qontinui-runner's .git lives at
    // ../.git relative to src-tauri.
    //   - .git/HEAD fires on branch switch / detached-head jumps.
    //   - .git/refs/heads/ (directory) fires on any new commit to any local
    //     branch, since refs/heads/<branch> is the file git updates when
    //     advancing a branch ref. Without this a fresh commit on the
    //     currently-checked-out branch would keep the old QONTINUI_GIT_SHA
    //     embedded, defeating the purpose of this stamp.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");

    tauri_build::build()
}
