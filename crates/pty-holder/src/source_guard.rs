//! Source-level byte-fidelity guard (plan Phase 1, amended bullet; idea:
//! superset-sh/superset).
//!
//! The runtime round-trip test proves the frame layer is byte-exact TODAY. This
//! guard fires the moment someone WRITES a text hop into the data path — a
//! re-encoding or a UTF-8 decode — rather than when a user first hits a
//! non-UTF-8 byte in production. It reads the crate's modules as text (with
//! `include_str!`, so the list is checked at compile time), strips comments,
//! and fails on any banned token.
//!
//! A second test reads `src/` at run time and fails when a `.rs` file is
//! neither guarded nor explicitly exempt, so a new module cannot be added
//! outside the guard by forgetting to list it.

/// The banned tokens. `from_utf8` also covers `from_utf8_lossy` and
/// `from_utf8_unchecked`; the lossy spellings are listed so a failure names the
/// exact call.
const BANNED: &[&str] = &["base64", "from_utf8", "from_utf8_lossy", "to_string_lossy"];

/// Every guarded module, by path relative to `src/`, with its text.
const GUARDED: &[(&str, &str)] = &[
    ("frame.rs", include_str!("frame.rs")),
    ("protocol.rs", include_str!("protocol.rs")),
    ("transport/mod.rs", include_str!("transport/mod.rs")),
    ("transport/unix.rs", include_str!("transport/unix.rs")),
    ("transport/windows.rs", include_str!("transport/windows.rs")),
    ("server.rs", include_str!("server.rs")),
    ("client.rs", include_str!("client.rs")),
    ("lock.rs", include_str!("lock.rs")),
    ("pane.rs", include_str!("pane.rs")),
    ("main.rs", include_str!("main.rs")),
    // Phase 2 data path: the PTY owner, its ring, the spec, the spawner.
    ("pty.rs", include_str!("pty.rs")),
    ("ring.rs", include_str!("ring.rs")),
    ("spec.rs", include_str!("spec.rs")),
    ("startup.rs", include_str!("startup.rs")),
    ("spawn.rs", include_str!("spawn.rs")),
];

/// Files deliberately outside the guard: this file (it must spell the banned
/// tokens) and `lib.rs` (module declarations and docs only).
const EXEMPT: &[&str] = &["source_guard.rs", "lib.rs"];

/// Remove `//` and (nested) `/* */` comments, keeping string and char
/// literals intact so a `"//"` inside a string is not mistaken for a comment.
fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 1;
                i += 2;
                while i < b.len() && depth > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                out.push(b' ');
            }
            // Raw string: r"..." / r#"..."# / br#"..."#.
            b'r' if matches!(b.get(i + 1), Some(b'"') | Some(b'#'))
                && (i == 0
                    || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')
                    || b[i - 1] == b'b') =>
            {
                let start = i;
                i += 1;
                let mut hashes = 0;
                while b.get(i) == Some(&b'#') {
                    hashes += 1;
                    i += 1;
                }
                if b.get(i) != Some(&b'"') {
                    // `r#ident` (a raw identifier), not a string.
                    out.extend_from_slice(&b[start..i]);
                    continue;
                }
                i += 1;
                loop {
                    if i >= b.len() {
                        break;
                    }
                    if b[i] == b'"'
                        && b[i + 1..].iter().take(hashes).all(|c| *c == b'#')
                        && b.len() - (i + 1) >= hashes
                    {
                        i += 1 + hashes;
                        break;
                    }
                    i += 1;
                }
                out.extend_from_slice(&b[start..i]);
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i = (i + 1).min(b.len());
                out.extend_from_slice(&b[start..i]);
            }
            // A char literal holding a quote, '"', must not open a string.
            b'\'' if b.get(i + 1) == Some(&b'"') && b.get(i + 2) == Some(&b'\'') => {
                out.extend_from_slice(&b[i..i + 3]);
                i += 3;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    // Byte-to-char widening, not a decode: a non-ASCII byte becomes some
    // Latin-1 char, which cannot create or destroy a match of the ASCII
    // tokens this is searched for.
    out.into_iter().map(char::from).collect::<String>()
}

fn violations(src: &str) -> Vec<&'static str> {
    let code = strip_comments(src);
    BANNED
        .iter()
        .copied()
        .filter(|t| code.contains(t))
        .collect()
}

#[test]
fn pty_holder_source_guard_no_text_hops_in_the_data_path() {
    let mut failures = Vec::new();
    for (name, text) in GUARDED {
        let v = violations(text);
        if !v.is_empty() {
            failures.push(format!("src/{name}: {v:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "text encoding/decoding in a pty-holder data-path module — pane bytes \
         are not UTF-8 and must travel raw (plan Phase 1): {failures:#?}"
    );
}

#[test]
fn pty_holder_source_guard_covers_every_module() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut unlisted = Vec::new();
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let rel: Vec<String> = path
                .strip_prefix(&src)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_str().unwrap().to_string())
                .collect();
            let rel = rel.join("/");
            let listed = GUARDED.iter().any(|(n, _)| *n == rel) || EXEMPT.contains(&rel.as_str());
            if !listed {
                unlisted.push(rel);
            }
        }
    }
    assert!(
        unlisted.is_empty(),
        "add these to source_guard::GUARDED (or, with a reason, EXEMPT): {unlisted:?}"
    );
}

#[test]
fn pty_holder_source_guard_strips_comments_not_code() {
    let banned = BANNED[0];
    assert!(violations(&format!(
        "// {banned}\n/* {banned} /* nested */ */ fn f() {{}}"
    ))
    .is_empty());
    assert_eq!(
        violations(&format!("let x = {banned}::encode(y);")),
        vec![banned]
    );
    // A "//" inside a string does not hide the code after it.
    assert_eq!(
        violations(&format!("let u = \"http://x\"; String::{}(v);", BANNED[1])),
        vec![BANNED[1]]
    );
    assert_eq!(
        violations(&format!("let p = r\"\\\\.\\pipe\\x\"; s.{}();", BANNED[3])),
        vec![BANNED[3]]
    );
}
