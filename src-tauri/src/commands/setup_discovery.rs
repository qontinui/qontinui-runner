//! Native (pure-Rust) implementations of the setup-wizard discovery commands.
//!
//! These are a faithful port of the offline discovery logic that used to live in
//! the vendored `qontinui_setup_mcp` Python package (`discovery/scanner.py`,
//! `discovery/frameworks.py`, `discovery/log_finder.py`). Porting them to Rust
//! removes the runner's dependency on an end-user Python 3.12+ interpreter — the
//! production build previously failed the first-launch wizard scan on any
//! machine without Python installed.
//!
//! The JSON output shapes are kept identical to the Python CLI so the existing
//! frontend consumers and `save_log_sources_from_setup` round-trip unchanged.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Resolve *path* to an absolute, symlink-free path (mirrors Python's
/// `Path(path).resolve()`). Returns `None` when the path does not exist, which
/// callers treat the same way Python treats a non-directory.
fn normalize_root(path: &str) -> Option<PathBuf> {
    let canon = std::fs::canonicalize(Path::new(path)).ok()?;
    Some(strip_verbatim(canon))
}

/// Strip the Windows `\\?\` verbatim prefix that `canonicalize` adds, so emitted
/// paths look like Python's `str(Path.resolve())` (`C:\...`, not `\\?\C:\...`).
fn strip_verbatim(p: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    p
}

/// Lowercase file name for a path (empty string when absent).
fn file_name_lower(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn file_name_str(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

// ===========================================================================
// scan_workspace — discover git-repo projects by manifest file
// ===========================================================================

/// Directories to skip during traversal.
const SCAN_SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "__pycache__",
    ".venv",
    "venv",
    ".env",
    "target",
    "dist",
    "build",
    ".next",
    ".cache",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
];

/// Ordered mapping of manifest filename → project type. Order matters: the first
/// present manifest wins (mirrors the Python dict insertion order).
const MANIFEST_MAP: &[(&str, &str)] = &[
    ("package.json", "node"),
    ("pyproject.toml", "python"),
    ("Cargo.toml", "rust"),
    ("go.mod", "go"),
    ("pom.xml", "java-maven"),
    ("build.gradle", "java-gradle"),
    ("build.gradle.kts", "java-gradle"),
    ("Gemfile", "ruby"),
    ("pubspec.yaml", "flutter"),
    ("composer.json", "php"),
];

/// File extensions (without the dot, case-sensitive to match `Path.suffix`) that
/// signal a .NET project.
const DOTNET_EXTS: &[&str] = &["sln", "csproj"];

/// Scan a directory tree for software projects.
///
/// Walks up to *max_depth* levels below *path*, reporting directories that are
/// git repositories (contain a `.git` directory) and carry a known manifest.
/// The scan root itself is never reported.
pub fn scan_workspace(path: &str, max_depth: u32) -> Vec<Value> {
    let root = match normalize_root(path) {
        Some(r) if r.is_dir() => r,
        _ => return Vec::new(),
    };
    let mut projects = Vec::new();
    walk_scan(&root, 0, max_depth, &mut projects);
    projects
}

fn walk_scan(dir: &Path, depth: u32, max_depth: u32, out: &mut Vec<Value>) {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };

    let mut subdirs: Vec<(String, PathBuf)> = Vec::new();
    let mut filenames: Vec<String> = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => subdirs.push((name, entry.path())),
            Ok(_) => filenames.push(name),
            Err(_) => {}
        }
    }

    // Check for .git BEFORE pruning skipped dirs.
    let has_git = subdirs.iter().any(|(n, _)| n == ".git");

    // Prune skipped directories and keep a stable (sorted) descent order.
    let mut kept: Vec<(String, PathBuf)> = subdirs
        .into_iter()
        .filter(|(n, _)| !SCAN_SKIP_DIRS.contains(&n.as_str()))
        .collect();
    kept.sort_by(|a, b| a.0.cmp(&b.0));

    // Only git-repo roots (other than the scan root itself) are real projects.
    if has_git && depth != 0 {
        let path_str = dir.to_string_lossy().to_string();
        let name = file_name_str(dir);
        let filenames_set: HashSet<&str> = filenames.iter().map(|s| s.as_str()).collect();

        let mut found = false;
        for (manifest_name, project_type) in MANIFEST_MAP {
            if filenames_set.contains(manifest_name) {
                out.push(json!({
                    "path": path_str,
                    "name": name,
                    "type": project_type,
                    "manifest": manifest_name,
                }));
                found = true;
                break;
            }
        }

        // .NET projects (*.sln, *.csproj).
        if !found {
            for filename in &filenames {
                let suffix = Path::new(filename).extension().and_then(|s| s.to_str());
                if let Some(ext) = suffix {
                    if DOTNET_EXTS.contains(&ext) {
                        out.push(json!({
                            "path": path_str,
                            "name": name,
                            "type": "dotnet",
                            "manifest": filename,
                        }));
                        break;
                    }
                }
            }
        }
    }

    // Stop descending beyond max_depth (but the current directory was checked).
    if depth < max_depth {
        for (_, sub) in &kept {
            walk_scan(sub, depth + 1, max_depth, out);
        }
    }
}

// ===========================================================================
// detect_framework — identify the best-matching framework for a project
// ===========================================================================

/// A framework's detection fingerprint and log conventions.
struct Fw {
    name: &'static str,
    key: &'static str,
    language: &'static str,
    category: &'static str,
    detection_files: &'static [&'static str],
    detection_deps: &'static [&'static str],
    manifest_file: &'static str,
    default_log_locations: &'static [&'static str],
    default_parser: &'static str,
    error_patterns: Vec<&'static str>,
    warning_patterns: Vec<&'static str>,
    keywords: &'static [&'static str],
    logs_to_file_by_default: bool,
    build_tool: Option<&'static str>,
}

const JS_ERR: &[&str] = &[
    r"(?i)\berror\b",
    r"ERR!",
    r"TypeError",
    r"ReferenceError",
    r"SyntaxError",
    r"RangeError",
    r"Unhandled(?:Rejection|Promise)",
    r"FATAL",
    r"Module not found",
];
const JS_WARN: &[&str] = &[
    r"(?i)\bwarn(?:ing)?\b",
    r"(?i)deprecated",
    r"ExperimentalWarning",
];

const PY_ERR: &[&str] = &[
    r"(?i)\berror\b",
    r"Traceback \(most recent call last\)",
    r"(?:Key|Value|Type|Attribute|Import|Runtime|OS)Error",
    r"FATAL",
    r"CRITICAL",
];
const PY_WARN: &[&str] = &[
    r"(?i)\bwarn(?:ing)?\b",
    r"(?i)deprecated",
    r"DeprecationWarning",
    r"FutureWarning",
];

const RUST_ERR: &[&str] = &[
    r"(?i)\berror\b",
    r"panicked at",
    r"thread '.*' panicked",
    r"FATAL",
    r"unwrap\(\) on .* value",
];
const RUST_WARN: &[&str] = &[r"(?i)\bwarn(?:ing)?\b", r"(?i)deprecated"];

const GEN_ERR: &[&str] = &[
    r"(?i)\berror\b",
    r"(?i)\bfatal\b",
    r"(?i)\bpanic\b",
    r"CRITICAL",
];
const GEN_WARN: &[&str] = &[r"(?i)\bwarn(?:ing)?\b", r"(?i)deprecated"];

fn concat(base: &[&'static str], extra: &[&'static str]) -> Vec<&'static str> {
    base.iter().chain(extra.iter()).copied().collect()
}

static REGISTRY: Lazy<Vec<Fw>> = Lazy::new(|| {
    vec![
        // ── Node.js / TypeScript ──────────────────────────────────────────
        Fw {
            name: "Next.js",
            key: "nextjs",
            language: "typescript",
            category: "fullstack",
            detection_files: &["next.config.js", "next.config.mjs", "next.config.ts"],
            detection_deps: &["next"],
            manifest_file: "package.json",
            default_log_locations: &[".next/server/logs"],
            default_parser: "javascript",
            error_patterns: JS_ERR.to_vec(),
            warning_patterns: JS_WARN.to_vec(),
            keywords: &[
                "next",
                "nextjs",
                "react",
                "ssr",
                "server-side rendering",
                "app router",
                "pages router",
                "middleware",
                "api route",
            ],
            logs_to_file_by_default: false,
            build_tool: Some("webpack/turbopack"),
        },
        Fw {
            name: "React/Vite",
            key: "react-vite",
            language: "typescript",
            category: "frontend",
            detection_files: &[],
            detection_deps: &["vite", "@vitejs/plugin-react"],
            manifest_file: "package.json",
            default_log_locations: &[],
            default_parser: "javascript",
            error_patterns: JS_ERR.to_vec(),
            warning_patterns: JS_WARN.to_vec(),
            keywords: &[
                "vite",
                "react",
                "hmr",
                "hot module replacement",
                "bundle",
                "rollup",
                "esbuild",
            ],
            logs_to_file_by_default: false,
            build_tool: Some("vite"),
        },
        Fw {
            name: "Express",
            key: "express",
            language: "typescript",
            category: "backend",
            detection_files: &[],
            detection_deps: &["express"],
            manifest_file: "package.json",
            default_log_locations: &["logs"],
            default_parser: "javascript",
            error_patterns: JS_ERR.to_vec(),
            warning_patterns: JS_WARN.to_vec(),
            keywords: &[
                "express",
                "middleware",
                "router",
                "http",
                "request",
                "response",
                "rest",
                "api",
            ],
            logs_to_file_by_default: false,
            build_tool: None,
        },
        Fw {
            name: "NestJS",
            key: "nestjs",
            language: "typescript",
            category: "backend",
            detection_files: &[],
            detection_deps: &["@nestjs/core"],
            manifest_file: "package.json",
            default_log_locations: &["logs"],
            default_parser: "javascript",
            error_patterns: JS_ERR.to_vec(),
            warning_patterns: JS_WARN.to_vec(),
            keywords: &[
                "nestjs",
                "nest",
                "decorator",
                "module",
                "controller",
                "provider",
                "guard",
                "interceptor",
                "pipe",
            ],
            logs_to_file_by_default: false,
            build_tool: None,
        },
        Fw {
            name: "React Native",
            key: "react-native",
            language: "typescript",
            category: "mobile",
            detection_files: &[],
            detection_deps: &["react-native"],
            manifest_file: "package.json",
            default_log_locations: &[],
            default_parser: "javascript",
            error_patterns: JS_ERR.to_vec(),
            warning_patterns: JS_WARN.to_vec(),
            keywords: &[
                "react-native",
                "metro",
                "bridge",
                "native module",
                "expo",
                "ios",
                "android",
            ],
            logs_to_file_by_default: false,
            build_tool: Some("metro"),
        },
        // ── Python ────────────────────────────────────────────────────────
        Fw {
            name: "Django",
            key: "django",
            language: "python",
            category: "fullstack",
            detection_files: &["manage.py"],
            detection_deps: &["django"],
            manifest_file: "pyproject.toml",
            default_log_locations: &["logs"],
            default_parser: "python",
            error_patterns: PY_ERR.to_vec(),
            warning_patterns: PY_WARN.to_vec(),
            keywords: &[
                "django",
                "manage.py",
                "wsgi",
                "asgi",
                "migration",
                "orm",
                "template",
                "middleware",
                "settings",
            ],
            logs_to_file_by_default: false,
            build_tool: None,
        },
        Fw {
            name: "Flask",
            key: "flask",
            language: "python",
            category: "backend",
            detection_files: &[],
            detection_deps: &["flask"],
            manifest_file: "pyproject.toml",
            default_log_locations: &["logs"],
            default_parser: "python",
            error_patterns: PY_ERR.to_vec(),
            warning_patterns: PY_WARN.to_vec(),
            keywords: &[
                "flask",
                "werkzeug",
                "jinja",
                "blueprint",
                "wsgi",
                "route",
                "endpoint",
            ],
            logs_to_file_by_default: false,
            build_tool: None,
        },
        Fw {
            name: "FastAPI",
            key: "fastapi",
            language: "python",
            category: "backend",
            detection_files: &[],
            detection_deps: &["fastapi"],
            manifest_file: "pyproject.toml",
            default_log_locations: &["logs"],
            default_parser: "python",
            error_patterns: PY_ERR.to_vec(),
            warning_patterns: PY_WARN.to_vec(),
            keywords: &[
                "fastapi",
                "uvicorn",
                "starlette",
                "pydantic",
                "openapi",
                "async",
                "endpoint",
                "router",
            ],
            logs_to_file_by_default: false,
            build_tool: None,
        },
        // ── Rust ──────────────────────────────────────────────────────────
        Fw {
            name: "Tauri",
            key: "tauri",
            language: "rust",
            category: "frontend",
            detection_files: &["tauri.conf.json", "tauri.conf.json5"],
            detection_deps: &["tauri"],
            manifest_file: "Cargo.toml",
            default_log_locations: &[],
            default_parser: "rust",
            error_patterns: RUST_ERR.to_vec(),
            warning_patterns: RUST_WARN.to_vec(),
            keywords: &[
                "tauri",
                "webview",
                "ipc",
                "invoke",
                "command",
                "window",
                "desktop",
                "system tray",
            ],
            logs_to_file_by_default: false,
            build_tool: Some("cargo"),
        },
        Fw {
            name: "Rust/Cargo",
            key: "rust-cargo",
            language: "rust",
            category: "system",
            detection_files: &["Cargo.toml"],
            detection_deps: &[],
            manifest_file: "Cargo.toml",
            default_log_locations: &[],
            default_parser: "rust",
            error_patterns: RUST_ERR.to_vec(),
            warning_patterns: RUST_WARN.to_vec(),
            keywords: &[
                "rust", "cargo", "crate", "trait", "impl", "borrow", "lifetime", "async", "tokio",
            ],
            logs_to_file_by_default: false,
            build_tool: Some("cargo"),
        },
        // ── Other ─────────────────────────────────────────────────────────
        Fw {
            name: "Go",
            key: "go",
            language: "go",
            category: "backend",
            detection_files: &["go.mod"],
            detection_deps: &[],
            manifest_file: "go.mod",
            default_log_locations: &[],
            default_parser: "generic",
            error_patterns: concat(GEN_ERR, &[r"goroutine \d+"]),
            warning_patterns: GEN_WARN.to_vec(),
            keywords: &[
                "go",
                "golang",
                "goroutine",
                "channel",
                "interface",
                "struct",
                "module",
                "package",
            ],
            logs_to_file_by_default: false,
            build_tool: None,
        },
        Fw {
            name: "Spring Boot",
            key: "spring-boot",
            language: "java",
            category: "backend",
            detection_files: &["pom.xml", "build.gradle"],
            detection_deps: &["org.springframework.boot"],
            manifest_file: "pom.xml",
            default_log_locations: &["logs", "target/logs"],
            default_parser: "generic",
            error_patterns: concat(
                GEN_ERR,
                &[
                    r"(?:java\.lang\.\w+)?Exception",
                    r"at [\w.$]+\([\w.]+:\d+\)",
                ],
            ),
            warning_patterns: GEN_WARN.to_vec(),
            keywords: &[
                "spring",
                "boot",
                "bean",
                "controller",
                "service",
                "repository",
                "autowired",
                "configuration",
            ],
            logs_to_file_by_default: true,
            build_tool: None,
        },
        Fw {
            name: "Rails",
            key: "rails",
            language: "ruby",
            category: "fullstack",
            detection_files: &["Gemfile", "config/routes.rb"],
            detection_deps: &["rails"],
            manifest_file: "Gemfile",
            default_log_locations: &["log"],
            default_parser: "generic",
            error_patterns: concat(
                GEN_ERR,
                &[r"ActionController::\w+Error", r"ActiveRecord::\w+Error"],
            ),
            warning_patterns: GEN_WARN.to_vec(),
            keywords: &[
                "rails",
                "ruby",
                "activerecord",
                "migration",
                "rake",
                "controller",
                "model",
                "view",
                "gem",
            ],
            logs_to_file_by_default: true,
            build_tool: None,
        },
        Fw {
            name: "Flutter",
            key: "flutter",
            language: "dart",
            category: "mobile",
            detection_files: &["pubspec.yaml"],
            detection_deps: &["flutter"],
            manifest_file: "pubspec.yaml",
            default_log_locations: &[],
            default_parser: "generic",
            error_patterns: concat(GEN_ERR, &[r"FlutterError", "════════"]),
            warning_patterns: GEN_WARN.to_vec(),
            keywords: &[
                "flutter",
                "dart",
                "widget",
                "stateful",
                "stateless",
                "build",
                "pubspec",
                "material",
                "cupertino",
            ],
            logs_to_file_by_default: false,
            build_tool: Some("flutter"),
        },
    ]
});

fn find_definition(key: &str) -> Option<&'static Fw> {
    REGISTRY.iter().find(|fw| fw.key == key)
}

/// Ordered list of manifests to probe; earlier entries take precedence.
const MANIFEST_PROBE_ORDER: &[&str] = &[
    "package.json",
    "pyproject.toml",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "pubspec.yaml",
];

fn language_from_manifest(m: &str) -> &'static str {
    match m {
        "package.json" => "typescript",
        "pyproject.toml" => "python",
        "Cargo.toml" => "rust",
        "go.mod" => "go",
        "pom.xml" => "java",
        "build.gradle" => "java",
        "build.gradle.kts" => "java",
        "Gemfile" => "ruby",
        "pubspec.yaml" => "dart",
        _ => "unknown",
    }
}

fn category_from_manifest(m: &str) -> &'static str {
    match m {
        "package.json" => "fullstack",
        "pyproject.toml" => "backend",
        "Cargo.toml" => "system",
        "go.mod" => "backend",
        "pom.xml" => "backend",
        "build.gradle" => "backend",
        "build.gradle.kts" => "backend",
        "Gemfile" => "backend",
        "pubspec.yaml" => "mobile",
        _ => "unknown",
    }
}

/// pip treats hyphens/underscores/dots as equivalent — normalise for comparison.
fn normalize_dep(d: &str) -> String {
    d.to_lowercase().replace(['-', '.'], "_")
}

fn unknown_result(reason: &str) -> Value {
    json!({
        "framework": "unknown",
        "key": "unknown",
        "language": "unknown",
        "category": "unknown",
        "build_tool": Value::Null,
        "logs_to_file_by_default": false,
        "detected_by": reason,
    })
}

/// Detect the primary framework used by a project.
pub fn detect_framework(project_path: &str) -> Value {
    let root = match normalize_root(project_path) {
        Some(r) if r.is_dir() => r,
        _ => return unknown_result("path is not a directory"),
    };

    // 1. Discover which manifests exist and read their text. `order` preserves
    //    insertion order (probe order first, then any _tauri_config markers) so
    //    the fallback "first manifest" matches Python's dict iteration.
    let mut order: Vec<String> = Vec::new();
    let mut text_map: HashMap<String, Option<String>> = HashMap::new();
    for manifest_name in MANIFEST_PROBE_ORDER {
        let p = root.join(manifest_name);
        if p.is_file() {
            order.push(manifest_name.to_string());
            text_map.insert(manifest_name.to_string(), std::fs::read_to_string(&p).ok());
        }
    }

    // Also check for a Tauri config in the project root or a src-tauri subdir.
    for tauri_config in ["tauri.conf.json", "tauri.conf.json5"] {
        for sub in ["", "src-tauri"] {
            let check = if sub.is_empty() {
                root.join(tauri_config)
            } else {
                root.join(sub).join(tauri_config)
            };
            if check.is_file() {
                let key = format!("_tauri_config:{}", tauri_config);
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    text_map.entry(key.clone())
                {
                    order.push(key);
                    entry.insert(Some(String::new()));
                }
            }
        }
    }

    if order.is_empty() {
        return unknown_result("no manifest files found");
    }

    // 2. Parse dependencies from manifests.
    let mut manifest_deps: HashMap<String, HashSet<String>> = HashMap::new();

    if let Some(text) = text_map.get("package.json") {
        let deps = text
            .as_ref()
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
            .map(|v| extract_node_deps(&v))
            .unwrap_or_default();
        manifest_deps.insert("package.json".to_string(), deps);
    }
    if let Some(text) = text_map.get("pyproject.toml") {
        let deps = text
            .as_ref()
            .map(|t| extract_pyproject_deps(t))
            .unwrap_or_default();
        manifest_deps.insert("pyproject.toml".to_string(), deps);
    }
    if let Some(text) = text_map.get("Cargo.toml") {
        let deps = text
            .as_ref()
            .map(|t| extract_cargo_deps(t))
            .unwrap_or_default();
        manifest_deps.insert("Cargo.toml".to_string(), deps);
    }
    if let Some(text) = text_map.get("Gemfile") {
        let deps = text
            .as_ref()
            .map(|t| extract_gemfile_deps(t))
            .unwrap_or_default();
        manifest_deps.insert("Gemfile".to_string(), deps);
    }
    if let Some(text) = text_map.get("pubspec.yaml") {
        let deps = text
            .as_ref()
            .map(|t| extract_pubspec_deps(t))
            .unwrap_or_default();
        manifest_deps.insert("pubspec.yaml".to_string(), deps);
    }
    if text_map.contains_key("go.mod") {
        manifest_deps
            .entry("go.mod".to_string())
            .or_insert_with(HashSet::new);
    }
    for gradle_name in ["pom.xml", "build.gradle", "build.gradle.kts"] {
        if text_map.contains_key(gradle_name) {
            manifest_deps
                .entry(gradle_name.to_string())
                .or_insert_with(HashSet::new);
        }
    }

    // 3. Score each framework in the registry (first best on ties).
    let mut best_score = 0i32;
    let mut best_fw: Option<&Fw> = None;
    let mut best_reason = String::new();

    for fw in REGISTRY.iter() {
        let (score, reason) = score_framework(fw, &root, &text_map, &manifest_deps);
        if score > best_score {
            best_score = score;
            best_fw = Some(fw);
            best_reason = reason;
        }
    }

    // 4. Build the result.
    if let Some(fw) = best_fw {
        return json!({
            "framework": fw.name,
            "key": fw.key,
            "language": fw.language,
            "category": fw.category,
            "build_tool": fw.build_tool,
            "logs_to_file_by_default": fw.logs_to_file_by_default,
            "detected_by": best_reason,
        });
    }

    // Fallback: derive language/category from the first manifest found.
    let first_manifest = &order[0];
    let clean_manifest = match first_manifest.rsplit_once(':') {
        Some((_, tail)) => tail,
        None => first_manifest.as_str(),
    };
    json!({
        "framework": "unknown",
        "key": "unknown",
        "language": language_from_manifest(clean_manifest),
        "category": category_from_manifest(clean_manifest),
        "build_tool": Value::Null,
        "logs_to_file_by_default": false,
        "detected_by": format!("manifest:{} (no framework matched)", clean_manifest),
    })
}

fn score_framework(
    fw: &Fw,
    root: &Path,
    text_map: &HashMap<String, Option<String>>,
    manifest_deps: &HashMap<String, HashSet<String>>,
) -> (i32, String) {
    let mut score = 0i32;
    let mut reasons: Vec<String> = Vec::new();

    // Manifest must be present for this framework to match.
    let mf = fw.manifest_file;
    if !mf.is_empty() && !text_map.contains_key(mf) {
        // Special case: build.gradle(.kts) counts for pom.xml manifests.
        if mf == "pom.xml" {
            if !text_map.contains_key("build.gradle") && !text_map.contains_key("build.gradle.kts")
            {
                return (0, String::new());
            }
        } else {
            return (0, String::new());
        }
    }

    // Detection files — each present file adds points (supports nested paths).
    for df in fw.detection_files {
        if root.join(df).exists() {
            score += 10;
            reasons.push(format!("file:{}", df));
        }
    }

    // Detection deps — each found dependency adds points (more specific).
    let empty = HashSet::new();
    let deps = manifest_deps.get(mf).unwrap_or(&empty);
    let deps_norm: HashSet<String> = deps.iter().map(|d| normalize_dep(d)).collect();
    for dd in fw.detection_deps {
        let dd_norm = normalize_dep(dd);
        if deps_norm.contains(&dd_norm) || deps.contains(*dd) {
            score += 20;
            reasons.push(format!("dep:{}", dd));
        } else {
            // For pom.xml / build.gradle, fall back to raw text search.
            for manifest_name in ["pom.xml", "build.gradle", "build.gradle.kts"] {
                if let Some(Some(text)) = text_map.get(manifest_name) {
                    if text.contains(*dd) {
                        score += 20;
                        reasons.push(format!("dep:{}(in {})", dd, manifest_name));
                        break;
                    }
                }
            }
        }
    }

    if score == 0 {
        return (0, String::new());
    }
    (score, reasons.join(", "))
}

// --- Manifest dependency extraction ---------------------------------------

fn extract_node_deps(data: &Value) -> HashSet<String> {
    let mut deps = HashSet::new();
    for section in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(obj) = data.get(section).and_then(|v| v.as_object()) {
            for k in obj.keys() {
                deps.insert(k.clone());
            }
        }
    }
    deps
}

fn extract_pyproject_deps(text: &str) -> HashSet<String> {
    let data: toml::Value = match toml::from_str(text) {
        Ok(v) => v,
        Err(_) => return HashSet::new(),
    };
    let mut deps: HashSet<String> = HashSet::new();

    // [tool.poetry.dependencies]
    if let Some(t) = data
        .get("tool")
        .and_then(|v| v.get("poetry"))
        .and_then(|v| v.get("dependencies"))
        .and_then(|v| v.as_table())
    {
        for k in t.keys() {
            deps.insert(k.clone());
        }
    }
    // [tool.poetry.group.*.dependencies]
    if let Some(groups) = data
        .get("tool")
        .and_then(|v| v.get("poetry"))
        .and_then(|v| v.get("group"))
        .and_then(|v| v.as_table())
    {
        for group in groups.values() {
            if let Some(gd) = group.get("dependencies").and_then(|v| v.as_table()) {
                for k in gd.keys() {
                    deps.insert(k.clone());
                }
            }
        }
    }
    // [project.dependencies] — PEP 621 (list of requirement strings)
    if let Some(arr) = data
        .get("project")
        .and_then(|v| v.get("dependencies"))
        .and_then(|v| v.as_array())
    {
        for req in arr {
            if let Some(s) = req.as_str() {
                let name = requirement_name(s);
                if !name.is_empty() {
                    deps.insert(name.to_lowercase());
                }
            }
        }
    }
    // [project.optional-dependencies]
    if let Some(opt) = data
        .get("project")
        .and_then(|v| v.get("optional-dependencies"))
        .and_then(|v| v.as_table())
    {
        for group_reqs in opt.values() {
            if let Some(a) = group_reqs.as_array() {
                for req in a {
                    if let Some(s) = req.as_str() {
                        let name = requirement_name(s);
                        if !name.is_empty() {
                            deps.insert(name.to_lowercase());
                        }
                    }
                }
            }
        }
    }

    // Normalise (pip treats hyphens/underscores/dots as equivalent).
    deps.iter().map(|d| normalize_dep(d)).collect()
}

/// Extract a package name from a PEP 508 requirement string (the part before any
/// version specifier, extras, marker, or whitespace).
fn requirement_name(req: &str) -> String {
    let idx = req
        .find(|c: char| matches!(c, '>' | '<' | '=' | '!' | '~' | ';' | '[') || c.is_whitespace());
    let name = match idx {
        Some(i) => &req[..i],
        None => req,
    };
    name.trim().to_string()
}

fn extract_cargo_deps(text: &str) -> HashSet<String> {
    let data: toml::Value = match toml::from_str(text) {
        Ok(v) => v,
        Err(_) => return HashSet::new(),
    };
    let mut deps = HashSet::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(t) = data.get(section).and_then(|v| v.as_table()) {
            for k in t.keys() {
                deps.insert(k.clone());
            }
        }
    }
    deps
}

static GEMFILE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"gem\s+['"]([^'"]+)['"]"#).unwrap());

fn extract_gemfile_deps(text: &str) -> HashSet<String> {
    let mut deps = HashSet::new();
    for cap in GEMFILE_RE.captures_iter(text) {
        deps.insert(cap[1].to_string());
    }
    deps
}

static PUBSPEC_SECTION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(dependencies|dev_dependencies)\s*:").unwrap());
static PUBSPEC_ITEM_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s+([\w_-]+)\s*:").unwrap());

fn extract_pubspec_deps(text: &str) -> HashSet<String> {
    // Faithful port of the Python implementation: the item regex is applied to a
    // whitespace-stripped line, so its leading `\s+` anchor makes it a no-op in
    // practice. Flutter is detected via `pubspec.yaml` presence, not this set.
    let mut deps = HashSet::new();
    let mut in_deps = false;
    for line in text.lines() {
        let stripped = line.trim();
        if PUBSPEC_SECTION_RE.is_match(stripped) {
            in_deps = true;
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') && stripped.contains(':') {
            in_deps = false;
            continue;
        }
        if in_deps {
            if let Some(c) = PUBSPEC_ITEM_RE.captures(stripped) {
                deps.insert(c[1].to_string());
            }
        }
    }
    deps
}

// ===========================================================================
// log_finder — find log files/dirs and suggest GlobalLogSource configs
// ===========================================================================

const LOG_SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "__pycache__",
    ".venv",
    "venv",
    "target",
    "dist",
    "build",
    ".next",
    ".cache",
];

const LOG_DIR_NAMES: &[&str] = &["logs", "log", ".logs", ".dev-logs"];

const COMMON_LOG_FILES: &[&str] = &[
    "debug.log",
    "error.log",
    "app.log",
    "access.log",
    "npm-debug.log",
    "yarn-error.log",
    "lerna-debug.log",
];

const VALID_CATEGORIES: &[&str] = &[
    "frontend", "backend", "api", "mobile", "database", "build", "testing", "runner", "general",
];

const DEFAULT_CATEGORY: &str = "general";

const LOG_MAX_DEPTH: u32 = 4;

fn format_guess(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("json") => "json",
        Some("jsonl") => "jsonl",
        _ => "plaintext",
    }
}

fn is_log_file(filename: &str) -> bool {
    let lower = filename.to_lowercase();

    if COMMON_LOG_FILES.contains(&lower.as_str()) {
        return true;
    }
    if lower.ends_with(".log") || lower.ends_with(".jsonl") || lower.ends_with(".err.log") {
        return true;
    }
    // Rotated logs like app.log.1, app.log.2, etc.
    if let Some((base, tail)) = lower.rsplit_once('.') {
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) && base.ends_with(".log") {
            return true;
        }
    }
    false
}

fn modified_iso(path: &Path) -> Option<String> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let dt: chrono::DateTime<chrono::Utc> = mtime.into();
    Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, false))
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn is_under_source_dir(path: &Path, root: &Path) -> bool {
    match path.strip_prefix(root) {
        Ok(rel) => rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().to_lowercase() == "src"),
        Err(_) => false,
    }
}

/// Scan a project directory for log files and log directories.
pub fn find_log_files(project_path: &str) -> Vec<Value> {
    let root = match normalize_root(project_path) {
        Some(r) if r.is_dir() => r,
        _ => return Vec::new(),
    };
    let mut results: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut log_dir_paths: HashSet<String> = HashSet::new();
    walk_logs(&root, 0, &root, &mut results, &mut seen, &mut log_dir_paths);
    results
}

fn walk_logs(
    dir: &Path,
    depth: u32,
    root: &Path,
    results: &mut Vec<Value>,
    seen: &mut HashSet<String>,
    log_dir_paths: &mut HashSet<String>,
) {
    // Prune beyond max depth (the directory is not processed at all).
    if depth >= LOG_MAX_DEPTH {
        return;
    }

    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut subdirs: Vec<(String, PathBuf)> = Vec::new();
    let mut filenames: Vec<String> = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => subdirs.push((name, entry.path())),
            Ok(_) => filenames.push(name),
            Err(_) => {}
        }
    }
    let mut kept: Vec<(String, PathBuf)> = subdirs
        .into_iter()
        .filter(|(n, _)| !LOG_SKIP_DIRS.contains(&n.as_str()))
        .collect();
    kept.sort_by(|a, b| a.0.cmp(&b.0));

    let name_lower = file_name_lower(dir);
    let is_root = dir == root;
    let mut skip_files = false;

    if LOG_DIR_NAMES.contains(&name_lower.as_str()) && !is_root {
        if name_lower == ".dev-logs" {
            // Handled at the workspace level; do not descend or scan files.
            return;
        }
        if is_under_source_dir(dir, root) {
            // src/.../logs is source code — skip files but still descend.
            skip_files = true;
        } else {
            let abs = dir.to_string_lossy().to_string();
            if seen.insert(abs.clone()) {
                log_dir_paths.insert(abs.clone());
                results.push(json!({
                    "path": abs,
                    "name": file_name_str(dir),
                    "type": "directory",
                    "size_bytes": 0,
                    "modified": modified_iso(dir),
                    "format_guess": "plaintext",
                }));
            }
            // Parent directory is now a log source — skip its individual files.
            skip_files = true;
        }
    }

    if !skip_files {
        for filename in &filenames {
            if is_log_file(filename) {
                let file_path = dir.join(filename);
                let abs = file_path.to_string_lossy().to_string();
                if seen.insert(abs.clone()) {
                    results.push(json!({
                        "path": abs,
                        "name": filename,
                        "type": "file",
                        "size_bytes": file_size(&file_path),
                        "modified": modified_iso(&file_path),
                        "format_guess": format_guess(&file_path),
                    }));
                }
            }
        }
    }

    for (_, sub) in &kept {
        walk_logs(sub, depth + 1, root, results, seen, log_dir_paths);
    }
}

fn map_framework_category(framework_category: Option<&str>) -> String {
    let cat = match framework_category {
        Some(c) => c,
        None => return DEFAULT_CATEGORY.to_string(),
    };
    let normalized = cat.to_lowercase();
    let normalized = normalized.trim();

    if VALID_CATEGORIES.contains(&normalized) {
        return normalized.to_string();
    }

    let mapped = match normalized {
        "fullstack" => "backend",
        "system" => "backend",
        "web-frontend" => "frontend",
        "web-backend" => "backend",
        "web" => "frontend",
        "server" => "backend",
        "rest-api" => "api",
        "graphql" => "api",
        "ios" => "mobile",
        "android" => "mobile",
        "react-native" => "mobile",
        "flutter" => "mobile",
        "db" => "database",
        "sql" => "database",
        "ci" => "build",
        "ci-cd" => "build",
        "bundler" => "build",
        "test" => "testing",
        "e2e" => "testing",
        "unit-test" => "testing",
        "automation" => "runner",
        "desktop" => "runner",
        _ => DEFAULT_CATEGORY,
    };
    mapped.to_string()
}

/// Build a single `GlobalLogSource`-compatible config dict.
#[allow(clippy::too_many_arguments)]
fn build_source(
    name: &str,
    description: &str,
    category: &str,
    source_type: &str,
    path: &str,
    format_guess: &str,
    keywords: &[String],
    parser: &str,
    error_patterns: &[String],
    warning_patterns: &[String],
    pattern: Option<&str>,
    color: Option<&str>,
) -> Value {
    let pattern_val = if source_type == "directory" {
        pattern.map(Value::from).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    json!({
        "id": Uuid::new_v4().to_string(),
        "name": name,
        "description": description,
        "category": category,
        "type": source_type,
        "path": path,
        "pattern": pattern_val,
        "tail_lines": 100,
        "enabled": true,
        "color": color.map(Value::from).unwrap_or(Value::Null),
        "keywords": keywords,
        "format": format_guess,
        "parser": parser,
        "timestamp_pattern": Value::Null,
        "timezone": "local",
        "error_patterns": error_patterns,
        "warning_patterns": warning_patterns,
        "ignore_patterns": Vec::<String>::new(),
        "poll_interval_ms": 5000,
    })
}

/// Combine framework detection and log-file discovery into suggested sources.
pub fn suggest_log_sources(project_path: &str) -> Value {
    let framework_result = detect_framework(project_path);
    let found_log_files = find_log_files(project_path);

    let framework_name = framework_result
        .get("framework")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();
    let framework_key = framework_result
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let framework_category = framework_result
        .get("category")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let framework_logs_to_files = framework_result
        .get("logs_to_file_by_default")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let def = find_definition(&framework_key);
    let keywords: Vec<String> = def
        .map(|d| d.keywords.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    let parser = def
        .map(|d| d.default_parser.to_string())
        .unwrap_or_else(|| "generic".to_string());
    let error_patterns: Vec<String> = def
        .map(|d| d.error_patterns.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    let warning_patterns: Vec<String> = def
        .map(|d| d.warning_patterns.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    let default_log_locations: Vec<String> = def
        .map(|d| {
            d.default_log_locations
                .iter()
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();

    let resolved = normalize_root(project_path);
    let project_name = resolved
        .as_ref()
        .map(|p| file_name_str(p))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| file_name_str(Path::new(project_path)));
    let mapped_category = map_framework_category(framework_category.as_deref());

    let mut suggested_sources: Vec<Value> = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();

    // 1. Sources from discovered log files / directories.
    for entry in &found_log_files {
        let entry_path = entry
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if !seen_paths.insert(entry_path.clone()) {
            continue;
        }
        let entry_name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let source_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or("file");
        let fmt = entry
            .get("format_guess")
            .and_then(|v| v.as_str())
            .unwrap_or("plaintext");

        suggested_sources.push(build_source(
            &format!("{} - {}", framework_name, entry_name),
            &format!("Auto-discovered log source for {}", project_name),
            &mapped_category,
            source_type,
            &entry_path,
            fmt,
            &keywords,
            &parser,
            &error_patterns,
            &warning_patterns,
            if source_type == "directory" {
                Some("*.log")
            } else {
                None
            },
            None,
        ));
    }

    // 2. Framework default log locations that exist but weren't discovered.
    if let Some(root) = &resolved {
        for rel_path in &default_log_locations {
            let log_path = root.join(rel_path);
            let abs = log_path.to_string_lossy().to_string();
            if seen_paths.contains(&abs) || !log_path.exists() {
                continue;
            }
            seen_paths.insert(abs.clone());

            let is_dir = log_path.is_dir();
            let entry_type = if is_dir { "directory" } else { "file" };
            let entry_format = if is_dir {
                "plaintext"
            } else {
                format_guess(&log_path)
            };

            suggested_sources.push(build_source(
                &format!("{} - {}", framework_name, file_name_str(&log_path)),
                &format!("Default log location for {}", framework_name),
                &mapped_category,
                entry_type,
                &abs,
                entry_format,
                &keywords,
                &parser,
                &error_patterns,
                &warning_patterns,
                if is_dir { Some("*.log") } else { None },
                None,
            ));
        }
    }

    let needs_logging_setup = !framework_logs_to_files && found_log_files.is_empty();

    json!({
        "framework": framework_result,
        "found_log_files": found_log_files,
        "suggested_sources": suggested_sources,
        "needs_logging_setup": needs_logging_setup,
    })
}

// --- suggest_workspace_sources --------------------------------------------

/// (exact filename, category, parser, display name)
const DEV_LOG_CLASSIFICATION: &[(&str, &str, &str, &str)] = &[
    ("backend.log", "backend", "python", "Backend"),
    ("backend.err.log", "backend", "python", "Backend Errors"),
    ("frontend.log", "frontend", "javascript", "Frontend"),
    (
        "frontend.err.log",
        "frontend",
        "javascript",
        "Frontend Errors",
    ),
    ("runner-tauri.log", "runner", "rust", "Runner"),
    (
        "runner-actions.jsonl",
        "runner",
        "generic",
        "Runner Actions",
    ),
    (
        "runner-general.jsonl",
        "runner",
        "generic",
        "Runner General",
    ),
    (
        "runner-image-recognition.jsonl",
        "runner",
        "generic",
        "Image Recognition",
    ),
    (
        "runner-playwright.jsonl",
        "testing",
        "generic",
        "Playwright",
    ),
    ("ai-output.jsonl", "runner", "generic", "AI Output"),
    ("supervisor.log", "runner", "generic", "Supervisor"),
    (
        "supervisor.err.log",
        "runner",
        "generic",
        "Supervisor Errors",
    ),
];

const DEV_LOG_SKIP_PATTERNS: &[&str] = &[
    "-copy",
    "-copy2",
    "-temp",
    "-temp2",
    "python-ws-debug",
    "backend-velocity",
    "logcat.log",
    "metro.log",
    "runner-build.log",
    "browser-events",
];

fn category_color(category: &str) -> Option<&'static str> {
    match category {
        "backend" => Some("#22c55e"),
        "frontend" => Some("#3b82f6"),
        "runner" => Some("#f97316"),
        "testing" => Some("#a855f7"),
        _ => None,
    }
}

/// Classify a dev-log file into (display_name, category, parser, format).
/// Returns `None` if the file should be skipped.
fn classify_dev_log(
    filename: &str,
) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    let lower = filename.to_lowercase();

    for skip in DEV_LOG_SKIP_PATTERNS {
        if lower.contains(skip) {
            return None;
        }
    }
    for (pattern_file, category, parser, display_name) in DEV_LOG_CLASSIFICATION {
        if lower == *pattern_file {
            let fmt = if lower.ends_with(".jsonl") {
                "jsonl"
            } else {
                "plaintext"
            };
            return Some((display_name, category, parser, fmt));
        }
    }
    None
}

/// Discover dev-log sources at the workspace root level (`.dev-logs/`).
pub fn suggest_workspace_sources(workspace_path: &str) -> Value {
    let root = match normalize_root(workspace_path) {
        Some(r) if r.is_dir() => r,
        _ => return json!({ "sources": [], "dev_logs_dir": Value::Null }),
    };

    let dev_logs_dir = root.join(".dev-logs");
    if !dev_logs_dir.is_dir() {
        return json!({ "sources": [], "dev_logs_dir": Value::Null });
    }

    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dev_logs_dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    entries.sort();

    let mut sources: Vec<Value> = Vec::new();
    for entry in entries {
        if !entry.is_file() {
            continue;
        }
        if file_size(&entry) == 0 {
            continue;
        }
        let fname = file_name_str(&entry);
        if let Some((display_name, category, parser, fmt)) = classify_dev_log(&fname) {
            let abs = entry.to_string_lossy().to_string();
            sources.push(build_source(
                display_name,
                &format!("Dev log from .dev-logs/{}", fname),
                category,
                "file",
                &abs,
                fmt,
                &[],
                parser,
                &[],
                &[],
                None,
                category_color(category),
            ));
        }
    }

    json!({
        "sources": sources,
        "dev_logs_dir": dev_logs_dir.to_string_lossy().to_string(),
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    // -- scan_workspace -----------------------------------------------------

    #[test]
    fn scan_reports_git_repo_projects_by_manifest() {
        let root = tempdir().unwrap();
        // A node project that is a git repo.
        let proj = root.path().join("my-app");
        fs::create_dir_all(proj.join(".git")).unwrap();
        write(&proj.join("package.json"), "{}");
        // A directory with a manifest but no .git — must NOT be reported.
        let non_git = root.path().join("libs");
        write(&non_git.join("Cargo.toml"), "[package]\nname=\"x\"");

        let projects = scan_workspace(&root.path().to_string_lossy(), 3);
        assert_eq!(projects.len(), 1, "only the git repo should be reported");
        let p = &projects[0];
        assert_eq!(p["name"], "my-app");
        assert_eq!(p["type"], "node");
        assert_eq!(p["manifest"], "package.json");
    }

    #[test]
    fn scan_skips_root_itself_and_skip_dirs() {
        let root = tempdir().unwrap();
        // Root is itself a git repo with a manifest — must not be reported.
        fs::create_dir_all(root.path().join(".git")).unwrap();
        write(&root.path().join("package.json"), "{}");
        // A project buried inside node_modules must be pruned.
        let buried = root.path().join("node_modules").join("dep");
        fs::create_dir_all(buried.join(".git")).unwrap();
        write(&buried.join("package.json"), "{}");

        let projects = scan_workspace(&root.path().to_string_lossy(), 3);
        assert!(projects.is_empty(), "root and pruned dirs yield nothing");
    }

    #[test]
    fn scan_detects_dotnet_projects() {
        let root = tempdir().unwrap();
        let proj = root.path().join("winapp");
        fs::create_dir_all(proj.join(".git")).unwrap();
        write(&proj.join("WinApp.csproj"), "<Project/>");
        let projects = scan_workspace(&root.path().to_string_lossy(), 3);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0]["type"], "dotnet");
        assert_eq!(projects[0]["manifest"], "WinApp.csproj");
    }

    // -- detect_framework ---------------------------------------------------

    #[test]
    fn detect_nextjs_via_dependency() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("package.json"),
            r#"{"dependencies": {"next": "14.0.0", "react": "18"}}"#,
        );
        let res = detect_framework(&dir.path().to_string_lossy());
        assert_eq!(res["key"], "nextjs");
        assert_eq!(res["language"], "typescript");
        assert_eq!(res["category"], "fullstack");
    }

    #[test]
    fn detect_tauri_via_cargo_dep_and_config() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("Cargo.toml"),
            "[dependencies]\ntauri = \"2\"\n",
        );
        write(&dir.path().join("src-tauri").join("tauri.conf.json"), "{}");
        let res = detect_framework(&dir.path().to_string_lossy());
        assert_eq!(res["key"], "tauri");
        assert_eq!(res["build_tool"], "cargo");
    }

    #[test]
    fn detect_fastapi_from_pep621_deps() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("pyproject.toml"),
            "[project]\nname=\"svc\"\ndependencies=[\"fastapi>=0.110\", \"uvicorn\"]\n",
        );
        let res = detect_framework(&dir.path().to_string_lossy());
        assert_eq!(res["key"], "fastapi");
        assert_eq!(res["language"], "python");
    }

    #[test]
    fn detect_unknown_when_no_manifest() {
        let dir = tempdir().unwrap();
        let res = detect_framework(&dir.path().to_string_lossy());
        assert_eq!(res["framework"], "unknown");
        assert_eq!(res["detected_by"], "no manifest files found");
    }

    #[test]
    fn detect_non_directory_path() {
        let res = detect_framework("/definitely/not/a/real/path/xyz123");
        assert_eq!(res["detected_by"], "path is not a directory");
    }

    #[test]
    fn detect_fallback_language_from_manifest() {
        let dir = tempdir().unwrap();
        // A bare pyproject.toml with no framework dependency and none of the
        // python frameworks' detection_files (e.g. manage.py) — exercises the
        // "manifest found but no framework matched" fallback branch.
        write(
            &dir.path().join("pyproject.toml"),
            "[project]\nname = \"lib\"\nversion = \"0.1.0\"\n",
        );
        let res = detect_framework(&dir.path().to_string_lossy());
        assert_eq!(res["framework"], "unknown");
        assert_eq!(res["language"], "python");
        assert_eq!(res["category"], "backend");
        assert_eq!(
            res["detected_by"],
            "manifest:pyproject.toml (no framework matched)"
        );
    }

    // -- dependency extraction ---------------------------------------------

    #[test]
    fn requirement_name_strips_specifiers() {
        assert_eq!(requirement_name("fastapi>=0.110"), "fastapi");
        assert_eq!(requirement_name("uvicorn[standard]"), "uvicorn");
        assert_eq!(requirement_name("pkg ; python_version < '3.12'"), "pkg");
        assert_eq!(requirement_name("bare"), "bare");
    }

    #[test]
    fn normalize_dep_canonicalises() {
        assert_eq!(normalize_dep("My-Pkg.Name"), "my_pkg_name");
    }

    // -- log discovery ------------------------------------------------------

    #[test]
    fn is_log_file_matches_expected() {
        assert!(is_log_file("app.log"));
        assert!(is_log_file("events.jsonl"));
        assert!(is_log_file("server.err.log"));
        assert!(is_log_file("app.log.3"));
        assert!(is_log_file("npm-debug.log"));
        assert!(!is_log_file("main.rs"));
        assert!(!is_log_file("app.log.notanumber"));
    }

    #[test]
    fn format_guess_by_extension() {
        assert_eq!(format_guess(Path::new("x.json")), "json");
        assert_eq!(format_guess(Path::new("x.jsonl")), "jsonl");
        assert_eq!(format_guess(Path::new("x.log")), "plaintext");
    }

    #[test]
    fn suggest_log_sources_finds_dir_and_reports_framework() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("package.json"),
            r#"{"dependencies": {"express": "4"}}"#,
        );
        // A logs directory and a stray log file.
        fs::create_dir_all(dir.path().join("logs")).unwrap();
        write(&dir.path().join("debug.log"), "line\n");

        let res = suggest_log_sources(&dir.path().to_string_lossy());
        assert_eq!(res["framework"]["key"], "express");
        let sources = res["suggested_sources"].as_array().unwrap();
        assert!(!sources.is_empty());
        // The logs directory should become a directory source with a *.log glob.
        let dir_source = sources
            .iter()
            .find(|s| s["type"] == "directory")
            .expect("a directory source");
        assert_eq!(dir_source["pattern"], "*.log");
        assert_eq!(dir_source["category"], "backend");
        // File sources carry a null pattern.
        let file_source = sources.iter().find(|s| s["type"] == "file").unwrap();
        assert!(file_source["pattern"].is_null());
    }

    #[test]
    fn suggest_workspace_sources_classifies_dev_logs() {
        let root = tempdir().unwrap();
        let dev = root.path().join(".dev-logs");
        write(&dev.join("backend.log"), "hi\n");
        write(&dev.join("frontend.err.log"), "boom\n");
        write(&dev.join("empty.log"), ""); // zero-byte, skipped
        write(&dev.join("logcat.log"), "x\n"); // skip-pattern, skipped

        let res = suggest_workspace_sources(&root.path().to_string_lossy());
        let sources = res["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 2, "only classified non-empty logs");
        let backend = sources.iter().find(|s| s["name"] == "Backend").unwrap();
        assert_eq!(backend["category"], "backend");
        assert_eq!(backend["parser"], "python");
        assert_eq!(backend["color"], "#22c55e");
        assert!(res["dev_logs_dir"].is_string());
    }

    #[test]
    fn suggest_workspace_sources_no_dev_logs_dir() {
        let root = tempdir().unwrap();
        let res = suggest_workspace_sources(&root.path().to_string_lossy());
        assert!(res["sources"].as_array().unwrap().is_empty());
        assert!(res["dev_logs_dir"].is_null());
    }

    #[test]
    fn map_framework_category_maps_and_defaults() {
        assert_eq!(map_framework_category(Some("backend")), "backend");
        assert_eq!(map_framework_category(Some("fullstack")), "backend");
        assert_eq!(map_framework_category(Some("weird")), "general");
        assert_eq!(map_framework_category(None), "general");
    }
}
