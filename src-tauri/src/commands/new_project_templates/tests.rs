use super::*;

#[test]
fn substitute_replaces_all_occurrences_of_both_params() {
    let out = substitute("{{name}} + {{app_id}} / {{name}}", "My.App", "my-app");
    assert_eq!(out, "My.App + my-app / My.App");
}

#[test]
fn substitute_leaves_jsx_braces_alone() {
    let out = substitute("metadata={{ description: \"{{name}}\" }}", "demo", "demo");
    assert_eq!(out, "metadata={{ description: \"demo\" }}");
}

#[test]
fn empty_template_has_readme_and_gitignore() {
    let files = template_files(TEMPLATE_EMPTY, "proj", "proj").unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path).collect();
    assert_eq!(paths, vec!["README.md", ".gitignore"]);
    assert_eq!(files[0].content, "# proj\n");
    assert!(files[1].content.contains("node_modules/"));
}

/// The runner's own `package.json`: the floor for the template's
/// `@qontinui/ui-bridge` pin, so a fresh scaffold never starts on an older
/// UI Bridge contract than the runner that created it.
const RUNNER_PACKAGE_JSON: &str = include_str!("../../../../package.json");

/// The design-kit tool CI's `design-guide` job installs, and the lock it
/// installs from. The template, this tool and the kit's own peers must agree
/// (`kit_chain_violations`), or a scaffold's first `npm install` is ERESOLVE.
const KIT_TOOL_PACKAGE_JSON: &str = include_str!("../../../../design/kit/package.json");
const KIT_TOOL_PACKAGE_LOCK: &str = include_str!("../../../../design/kit/package-lock.json");

/// Replace the `github:qontinui/qontinui-design-kit#v<tag>` spec in a
/// package.json text with `spec`, whatever the tag currently is.
fn replace_kit_ref(c: &str, spec: &str) -> String {
    let marker = "qontinui-design-kit#v";
    let Some(hit) = c.find(marker) else {
        return c.to_string();
    };
    let start = c[..hit].rfind('"').expect("kit ref is a quoted string") + 1;
    let end = hit + c[hit..].find('"').expect("kit ref is a quoted string");
    format!("{}{spec}{}", &c[..start], &c[end..])
}

/// Every path the design-bearing template must write.
const REACT_PATHS: [&str; 21] = [
    "package.json",
    "vite.config.ts",
    "tsconfig.json",
    "index.html",
    "specimen.html",
    "src/main.tsx",
    "src/App.tsx",
    "src/index.css",
    "design/language.dtcg.json",
    "src/design/tokens.css",
    "src/design/tokens.ts",
    "src/design/theme.css",
    "src/design/compile-report.json",
    "src/design/design-budgets.json",
    "src/design/visual-system.thresholds.json",
    "src/design/app.styleguide.uibridge.json",
    "specs/.gitkeep",
    "README.md",
    ".gitignore",
    "CLAUDE.md",
    ".github/workflows/ci.yml",
];

const INDEX_START: &str = "<!-- design-kit-index:start -->";
const INDEX_END: &str = "<!-- design-kit-index:end -->";

/// `^1.2.3` / `~1.2.3` / `1.2.3` -> (1, 2, 3). `None` for anything else
/// (a git ref, a tag, a range with an upper bound).
fn version_floor(range: &str) -> Option<(u64, u64, u64)> {
    let v = range.trim_start_matches(['^', '~', '=', ' ']);
    let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
    let floor = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(floor)
}

/// `github:<owner>/<repo>#v<major>.<minor>.<patch>` — a git dependency
/// pinned to a release tag (never a branch, a sha, or a registry range).
fn is_git_tag_ref(spec: &str) -> bool {
    let Some(rest) = spec.strip_prefix("github:") else {
        return false;
    };
    let Some((_repo, tag)) = rest.split_once('#') else {
        return false;
    };
    tag.strip_prefix('v').and_then(version_floor).is_some() && !tag.contains(['^', '~'])
}

/// Every leaf (an object carrying `$extensions.qontinui.source`) under
/// `value`, as (dotted path, source).
fn source_leaves(value: &serde_json::Value, path: &str, out: &mut Vec<(String, String)>) {
    let Some(obj) = value.as_object() else {
        return;
    };
    if let Some(src) = obj
        .get("$extensions")
        .and_then(|e| e.get("qontinui"))
        .and_then(|q| q.get("source"))
        .and_then(|s| s.as_str())
    {
        out.push((path.to_string(), src.to_string()));
    }
    for (k, v) in obj {
        if !k.starts_with('$') {
            let child = if path.is_empty() {
                k.clone()
            } else {
                format!("{path}.{k}")
            };
            source_leaves(v, &child, out);
        }
    }
}

/// Everything the design-bearing template must satisfy, as named
/// violations. Empty means conforming. The seeded-violation test below
/// reddens each check, so none of them is vacuous.
fn design_violations(files: &[TemplateFile], runner_package_json: &str) -> Vec<String> {
    let mut bad = Vec::new();
    let get = |p: &str| {
        files
            .iter()
            .find(|f| f.path == p)
            .map(|f| f.content.as_str())
    };
    for p in REACT_PATHS {
        if get(p).is_none() {
            bad.push(format!("missing path {p}"));
        }
    }
    for f in files {
        if f.content.contains("{{name}}") || f.content.contains("{{app_id}}") {
            bad.push(format!("placeholder left in {}", f.path));
        }
    }

    let pkg: serde_json::Value = get("package.json")
        .and_then(|c| serde_json::from_str(c).ok())
        .unwrap_or(serde_json::Value::Null);
    let build = pkg["scripts"]["build"].as_str().unwrap_or("");
    for step in [
        "design-kit compile --check",
        "design-kit specimen --check",
        "design-kit index --check",
        "design-kit lint-classes src",
        "tsc --noEmit",
        "vite build",
    ] {
        if !build.contains(step) {
            bad.push(format!("build script lacks `{step}`"));
        }
    }
    let kit = pkg["dependencies"]["@qontinui/design-kit"]
        .as_str()
        .unwrap_or("");
    if !is_git_tag_ref(kit) {
        bad.push(format!(
            "design-kit dependency `{kit}` is not a git ref in tag form"
        ));
    }
    let runner: serde_json::Value =
        serde_json::from_str(runner_package_json).unwrap_or(serde_json::Value::Null);
    let floor = runner["dependencies"]["@qontinui/ui-bridge"]
        .as_str()
        .and_then(version_floor);
    let pin = pkg["dependencies"]["@qontinui/ui-bridge"]
        .as_str()
        .and_then(version_floor);
    match (pin, floor) {
        (Some(p), Some(f)) if p >= f => {}
        (p, f) => bad.push(format!("ui-bridge pin {p:?} is below the runner's {f:?}")),
    }
    let pin_spec = pkg["dependencies"]["@qontinui/ui-bridge"]
        .as_str()
        .unwrap_or("");
    if !pin_spec.starts_with(|c: char| c.is_ascii_digit()) {
        bad.push(format!(
            "ui-bridge pin `{pin_spec}` is not an exact version"
        ));
    }
    for dep in ["tailwindcss", "@tailwindcss/vite"] {
        let spec = pkg["devDependencies"][dep].as_str().unwrap_or("");
        if version_floor(spec).map(|v| v.0) != Some(4) {
            bad.push(format!("{dep} `{spec}` is not Tailwind v4"));
        }
    }

    let source: serde_json::Value = get("design/language.dtcg.json")
        .and_then(|c| serde_json::from_str(c).ok())
        .unwrap_or(serde_json::Value::Null);
    let mut leaves = Vec::new();
    source_leaves(&source, "", &mut leaves);
    if leaves.is_empty() {
        bad.push("language source declares no leaves".to_string());
    }
    for (path, src) in &leaves {
        let identity = path.starts_with("color")
            || path.starts_with("type.family")
            || path.starts_with("imagery");
        if identity && src.starts_with("authored") {
            bad.push(format!("identity leaf {path} is agent-authored"));
        }
        if let Some(inner) = src.strip_prefix("authored(") {
            if inner.trim_end_matches(')').is_empty() {
                bad.push(format!("authored leaf {path} carries no notification_ref"));
            }
        } else if src == "authored" {
            bad.push(format!("authored leaf {path} carries no notification_ref"));
        }
    }

    let claude = get("CLAUDE.md").unwrap_or("");
    match (claude.find(INDEX_START), claude.find(INDEX_END)) {
        (Some(s), Some(e)) if s < e => {}
        _ => bad.push("CLAUDE.md lacks the index markers in order".to_string()),
    }

    let ci = get(".github/workflows/ci.yml").unwrap_or("");
    for (needle, why) in [
        ("- run: npm install", "must run npm install"),
        ("- run: npm run build", "must run npm run build"),
    ] {
        if !ci.contains(needle) {
            bad.push(format!("ci.yml {why}"));
        }
    }
    for (needle, why) in [
        ("npm ci", "npm ci fails on a scaffold with no lockfile"),
        ("cache:", "a lockfile-keyed cache fails with no lockfile"),
        ("git diff", "a tree-cleanliness step fails by construction"),
        ("${{", "Actions expressions stay out of a templated file"),
    ] {
        if ci.contains(needle) {
            bad.push(format!("ci.yml contains `{needle}`: {why}"));
        }
    }
    bad
}

fn react_files() -> Vec<TemplateFile> {
    template_files(TEMPLATE_REACT_UI_BRIDGE, "My-App", "my-app").unwrap()
}

#[test]
fn react_template_is_fully_parameterized() {
    let files = react_files();
    let paths: Vec<&str> = files.iter().map(|f| f.path).collect();
    for expected in REACT_PATHS {
        assert!(paths.contains(&expected), "missing {}", expected);
    }
    assert_eq!(paths.len(), REACT_PATHS.len(), "unexpected extra paths");
    let violations = design_violations(&files, RUNNER_PACKAGE_JSON);
    assert!(violations.is_empty(), "{violations:#?}");

    let pkg = files.iter().find(|f| f.path == "package.json").unwrap();
    assert!(pkg.content.contains("\"name\": \"my-app\""));
    assert!(pkg.content.contains("@qontinui/ui-bridge"));
    assert!(pkg.content.contains("@qontinui/ui-bridge-auto"));
    let vite = files.iter().find(|f| f.path == "vite.config.ts").unwrap();
    assert!(vite
        .content
        .contains("import { uiBridgeIRPlugin } from \"@qontinui/ui-bridge-auto/ir-builder\""));
    assert!(vite.content.contains("documentId: \"my-app\""));
    assert!(vite.content.contains("specimen.html"));
    let app = files.iter().find(|f| f.path == "src/App.tsx").unwrap();
    assert!(app.content.contains("<State"));
    assert!(app.content.contains("id=\"home\""));
    // Every state of the page is handled: the four required props.
    for prop in ["empty=", "loading=", "error=", "filteredEmpty="] {
        assert!(app.content.contains(prop), "App.tsx lacks {prop}");
    }
    // The compiled theme is exclusive: it clears every default first.
    let theme = files
        .iter()
        .find(|f| f.path == "src/design/theme.css")
        .unwrap();
    assert!(theme.content.contains("--*: initial;"));
    // The undeclared identity says so on the specimen's every frame.
    let specimen = files.iter().find(|f| f.path == "specimen.html").unwrap();
    assert!(specimen
        .content
        .contains("data-design-identity=\"undeclared\""));
    // The CI workflow is what makes a scaffolded repo's PRs blockable at
    // all; its measured constraints are pinned here so a later "cleanup"
    // cannot quietly break a bare scaffold's build.
    let ci = files
        .iter()
        .find(|f| f.path == ".github/workflows/ci.yml")
        .unwrap();
    assert!(ci.content.contains("- run: npm install"));
    assert!(ci.content.contains("- run: npm run build"));
    // No lockfile ships, so `npm ci` and setup-node's lockfile-keyed cache
    // both fail on a bare scaffold.
    assert!(!ci.content.contains("npm ci"));
    assert!(!ci.content.contains("cache:"));
    // `vite build` writes src/state-machine.derived.json, so a
    // tree-cleanliness step would fail by construction.
    assert!(!ci.content.contains("git diff"));
    // Keep Actions' expression syntax out of a file this module templates.
    assert!(!ci.content.contains("${{"));
}

/// Each seeded fixture breaks exactly one rule and must redden the check
/// that names it: a check that cannot fail proves nothing.
#[test]
fn design_checks_redden_on_each_seeded_violation() {
    let mutate = |path: &str, f: &dyn Fn(&str) -> String| -> Vec<String> {
        let mut files = react_files();
        let file = files.iter_mut().find(|x| x.path == path).unwrap();
        file.content = f(&file.content);
        design_violations(&files, RUNNER_PACKAGE_JSON)
    };
    let reddens = |violations: Vec<String>, needle: &str| {
        assert!(
            violations.iter().any(|v| v.contains(needle)),
            "expected a violation containing `{needle}`, got {violations:#?}"
        );
    };
    assert!(design_violations(&react_files(), RUNNER_PACKAGE_JSON).is_empty());

    let mut missing = react_files();
    missing.retain(|f| f.path != "specimen.html");
    reddens(
        design_violations(&missing, RUNNER_PACKAGE_JSON),
        "missing path specimen.html",
    );

    reddens(
        mutate("src/App.tsx", &|c| format!("{c}\n{{{{name}}}}")),
        "placeholder left in src/App.tsx",
    );
    reddens(
        mutate("package.json", &|c| {
            c.replace("design-kit compile --check", "design-kit compile")
        }),
        "lacks `design-kit compile --check`",
    );
    reddens(
        mutate("package.json", &|c| {
            c.replace("design-kit specimen --check && ", "")
        }),
        "lacks `design-kit specimen --check`",
    );
    reddens(
        mutate("package.json", &|c| {
            c.replace("design-kit lint-classes src && ", "")
        }),
        "lacks `design-kit lint-classes src`",
    );
    for bad in [
        "github:qontinui/qontinui-design-kit#main",
        "^0.1.0",
        "github:qontinui/qontinui-design-kit",
    ] {
        reddens(
            // Version-agnostic: replace whatever tagged ref the kit pin
            // carries, so a kit tag bump cannot turn this seed into a no-op.
            mutate("package.json", &|c| {
                let out = replace_kit_ref(c, bad);
                assert_ne!(out, c, "kit-ref seed `{bad}` changed nothing");
                out
            }),
            "not a git ref in tag form",
        );
    }
    reddens(
        mutate("package.json", &|c| {
            let pin = format!("\"@qontinui/ui-bridge\": \"{}\"", "^0.21.1");
            let at = c.find("\"@qontinui/ui-bridge\": \"").unwrap();
            let (head, rest) = c.split_at(at);
            let (_, tail) = rest.split_at(rest.find(',').unwrap());
            format!("{head}{pin}{tail}")
        }),
        "is below the runner's",
    );
    reddens(
        mutate("package.json", &|c| {
            c.replace("\"tailwindcss\": \"^4", "\"tailwindcss\": \"^3")
        }),
        "is not Tailwind v4",
    );
    reddens(
        mutate("package.json", &|c| {
            c.replace("\"@tailwindcss/vite\": \"^4", "\"@tailwindcss/vite\": \"^3")
        }),
        "@tailwindcss/vite `^3",
    );
    for step in ["design-kit index --check && ", "tsc --noEmit && "] {
        reddens(
            mutate("package.json", &|c| c.replace(step, "")),
            &format!("lacks `{}`", step.trim_end_matches(" && ")),
        );
    }
    reddens(
        mutate("package.json", &|c| c.replace(" && vite build", "")),
        "lacks `vite build`",
    );
    reddens(
        // Version-agnostic: put a caret in front of whatever the pin is,
        // so a ui-bridge bump cannot turn this seed into a no-op.
        mutate("package.json", &|c| {
            let key = "\"@qontinui/ui-bridge\": \"";
            let at = c.find(key).unwrap() + key.len();
            let (head, tail) = c.split_at(at);
            format!("{head}^{tail}")
        }),
        "is not an exact version",
    );
    reddens(
        mutate("design/language.dtcg.json", &|_| "{}".to_string()),
        "language source declares no leaves",
    );
    // An agent-authored hue.
    reddens(
        mutate("design/language.dtcg.json", &|c| {
            c.replacen(
                "\"source\": \"undeclared\"",
                "\"source\": \"authored(x)\"",
                1,
            )
        }),
        "identity leaf color.role.surface is agent-authored",
    );
    // An agent-authored family (the second undeclared leaf is
    // `type.family.sans`). The template carries no imagery leaf, so the
    // `imagery` disjunct has no seed here.
    reddens(
        mutate("design/language.dtcg.json", &|c| {
            let needle = "\"source\": \"undeclared\"";
            let (at, _) = c.match_indices(needle).nth(1).unwrap();
            let (head, rest) = c.split_at(at);
            let tail = rest.strip_prefix(needle).unwrap();
            format!("{head}\"source\": \"authored(x)\"{tail}")
        }),
        "identity leaf type.family.sans is agent-authored",
    );
    // A bare `authored` source, with no parenthesised reference at all.
    reddens(
        mutate("design/language.dtcg.json", &|c| {
            c.replacen(
                "authored(a784185b-4f36-4ad1-b57a-30ef4fbfc9ee)",
                "authored",
                1,
            )
        }),
        "carries no notification_ref",
    );
    // An authored leaf with no notification reference.
    reddens(
        mutate("design/language.dtcg.json", &|c| {
            c.replacen(
                "authored(a784185b-4f36-4ad1-b57a-30ef4fbfc9ee)",
                "authored()",
                1,
            )
        }),
        "carries no notification_ref",
    );
    reddens(
        mutate("CLAUDE.md", &|c| c.replace(INDEX_END, "")),
        "lacks the index markers",
    );
    reddens(
        mutate("CLAUDE.md", &|c| {
            c.replace(INDEX_START, "\0")
                .replace(INDEX_END, INDEX_START)
                .replace('\0', INDEX_END)
        }),
        "lacks the index markers",
    );
    // Each ci.yml mutation must redden the rule it seeds, named.
    for (needle, replacement, expected) in [
        (
            "- run: npm install",
            "- run: npm ci",
            "ci.yml contains `npm ci`",
        ),
        (
            "node-version: '24'",
            "node-version: '24'\n          cache: npm",
            "ci.yml contains `cache:`",
        ),
        (
            "- run: npm run build",
            "- run: npm run build\n      - run: git diff --exit-code",
            "ci.yml contains `git diff`",
        ),
        (
            "- run: npm run build",
            "- run: npm run build\n      - run: echo ${{ github.sha }}",
            "ci.yml contains `${{`",
        ),
        (
            "- run: npm run build",
            "- run: npm test",
            "ci.yml must run npm run build",
        ),
        (
            "- run: npm install",
            "- run: npm i",
            "ci.yml must run npm install",
        ),
    ] {
        reddens(
            mutate(".github/workflows/ci.yml", &|c| {
                c.replace(needle, replacement)
            }),
            expected,
        );
    }
}

/// Parse the lower bound of a `^X.Y.Z` / `~X.Y.Z` / `X.Y.Z` npm range.
fn range_floor(range: &str) -> semver::Version {
    let bare = range.trim().trim_start_matches(['^', '~', '=']);
    semver::Version::parse(bare).unwrap_or_else(|e| panic!("unparseable npm range {range:?}: {e}"))
}

fn dep_range(pkg: &serde_json::Value, name: &str) -> Option<String> {
    ["dependencies", "devDependencies"]
        .iter()
        .find_map(|section| pkg.get(section)?.get(name)?.as_str())
        .map(str::to_string)
}

/// Does `version` satisfy the npm range `range`? `None` when the range is not
/// one this translation understands — a git ref, a dist-tag, a hyphen range,
/// a wildcard — so a caller reports it rather than reading it as satisfied.
///
/// npm and the `semver` crate disagree on a bare `X.Y.Z` (npm: exact; the
/// crate: `^X.Y.Z`) and on separators (npm: whitespace within a set; the
/// crate: `, `), so each comparator set is rewritten before parsing.
fn npm_range_satisfied(range: &str, version: &str) -> Option<bool> {
    let version = semver::Version::parse(version.trim()).ok()?;
    let mut satisfied = false;
    for set in range.split("||") {
        let mut comparators: Vec<String> = Vec::new();
        let mut pending_op = String::new();
        for token in set.split_whitespace() {
            // npm allows `>= 1.2.3`; glue a lone operator to its version.
            if token
                .chars()
                .all(|c| matches!(c, '<' | '>' | '=' | '^' | '~'))
            {
                pending_op.push_str(token);
                continue;
            }
            let token = format!("{}{token}", std::mem::take(&mut pending_op));
            let bare = token.trim_start_matches(['<', '>', '=', '^', '~', 'v']);
            // Wildcards assert nothing; hyphen ranges and tags do not translate.
            if bare.is_empty()
                || bare
                    .split('.')
                    .any(|part| matches!(part, "*" | "x" | "X") || part.contains('-'))
            {
                return None;
            }
            if token.starts_with(|c: char| c.is_ascii_digit()) {
                comparators.push(format!("={token}"));
            } else {
                comparators.push(token);
            }
        }
        if comparators.is_empty() || !pending_op.is_empty() {
            return None;
        }
        let req = semver::VersionReq::parse(&comparators.join(", ")).ok()?;
        if req.comparators.iter().any(|c| c.op == semver::Op::Wildcard) {
            return None;
        }
        satisfied |= req.matches(&version);
    }
    Some(satisfied)
}

/// The three UI Bridge packages the kit declares as (optional) peers and the
/// template and the kit tool both pin.
const UI_BRIDGE_PACKAGES: [&str; 3] = [
    "@qontinui/ui-bridge",
    "@qontinui/ui-bridge-headless",
    "@qontinui/ui-bridge-wrapper",
];

/// Everything that ties the New Project template, the kit tool CI installs
/// (`design/kit/package.json` + its lock) and the kit's own peer ranges
/// together, as named violations. Empty means a scaffold installs the very
/// combination CI's `design-guide` job proves.
fn kit_chain_violations(template_pkg: &str, tool_pkg: &str, tool_lock: &str) -> Vec<String> {
    const KIT: &str = "@qontinui/design-kit";
    let mut bad = Vec::new();
    let parse = |what: &str, text: &str, bad: &mut Vec<String>| {
        serde_json::from_str::<serde_json::Value>(text)
            .map_err(|e| bad.push(format!("unparseable {what}: {e}")))
            .ok()
    };
    let (Some(template), Some(tool), Some(lock)) = (
        parse("template package.json", template_pkg, &mut bad),
        parse("kit tool package.json", tool_pkg, &mut bad),
        parse("kit tool package-lock.json", tool_lock, &mut bad),
    ) else {
        return bad;
    };

    // Rule 1: the template and the tool install the same kit.
    let tool_kit = dep_range(&tool, KIT);
    let template_kit = dep_range(&template, KIT);
    if tool_kit.is_none() || template_kit != tool_kit {
        bad.push(format!(
            "rule 1 (kit ref): template pins {KIT} {template_kit:?}, the kit tool {tool_kit:?}"
        ));
    }

    // Rule 2: the template ships exactly the UI Bridge versions the tool
    // proves, and the tool's pins are exact.
    for name in UI_BRIDGE_PACKAGES {
        let tool_pin = dep_range(&tool, name);
        let template_pin = dep_range(&template, name);
        if tool_pin.is_none() || template_pin != tool_pin {
            bad.push(format!(
                "rule 2 (ui-bridge pins): template pins {name} {template_pin:?}, the kit tool {tool_pin:?}"
            ));
        }
        if let Some(pin) = &tool_pin {
            if !pin.starts_with(|c: char| c.is_ascii_digit()) || version_floor(pin).is_none() {
                bad.push(format!(
                    "rule 2 (ui-bridge pins): the kit tool pins {name} `{pin}`, not an exact version"
                ));
            }
        }
    }

    // Rule 3: the lock was regenerated for the tool's current dependencies.
    let lock_root = lock
        .pointer("/packages/")
        .and_then(|root| root.get("dependencies"));
    if lock_root.is_none() || lock_root != tool.get("dependencies") {
        bad.push(format!(
            "rule 3 (stale lock): package-lock.json root dependencies {lock_root:?} differ from the kit tool's {:?}",
            tool.get("dependencies")
        ));
    }

    // Rule 4: the lock's kit entry is the tool's tag, resolved to a sha.
    let lock_kit = lock
        .get("packages")
        .and_then(|p| p.get(format!("node_modules/{KIT}")));
    let resolved = lock_kit
        .and_then(|k| k.get("resolved"))
        .and_then(|r| r.as_str())
        .unwrap_or_default();
    let sha = resolved
        .rsplit_once('#')
        .map(|(_, s)| s)
        .unwrap_or_default();
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        bad.push(format!(
            "rule 4 (lock kit entry): {KIT} resolved `{resolved}` does not end in a 40-hex sha"
        ));
    }
    let tool_tag = tool_kit
        .as_deref()
        .and_then(|spec| spec.rsplit_once("#v"))
        .map(|(_, tag)| tag.to_string());
    let lock_version = lock_kit
        .and_then(|k| k.get("version"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if tool_tag.is_none() || lock_version != tool_tag {
        bad.push(format!(
            "rule 4 (lock kit entry): {KIT} locked at version {lock_version:?}, the kit tool's tag is {tool_tag:?}"
        ));
    }

    // Rule 5: each exact tool pin satisfies the kit's recorded peer range.
    for name in UI_BRIDGE_PACKAGES {
        let Some(pin) = dep_range(&tool, name) else {
            continue; // reported by rule 2
        };
        let peer = lock_kit
            .and_then(|k| k.get("peerDependencies"))
            .and_then(|p| p.get(name))
            .and_then(|r| r.as_str());
        match peer.map(|range| (range, npm_range_satisfied(range, &pin))) {
            Some((_, Some(true))) => {}
            Some((range, Some(false))) => bad.push(format!(
                "rule 5 (kit peer): the kit's peer {name} `{range}` rejects the pinned `{pin}`"
            )),
            Some((range, None)) => bad.push(format!(
                "rule 5 (kit peer): the kit's peer {name} `{range}` (or the pin `{pin}`) cannot be checked"
            )),
            None => bad.push(format!(
                "rule 5 (kit peer): the locked kit records no peer for {name}"
            )),
        }
    }
    bad
}

#[test]
fn npm_range_satisfied_reads_npm_grammar() {
    // The eight cases the plan names.
    assert_eq!(npm_range_satisfied("0.54.0", "0.56.0"), Some(false));
    assert_eq!(npm_range_satisfied("0.54.0", "0.54.0"), Some(true));
    assert_eq!(npm_range_satisfied(">=0.54.0 <1", "0.56.0"), Some(true));
    assert_eq!(npm_range_satisfied(">=0.54.0 <1", "1.0.0"), Some(false));
    assert_eq!(npm_range_satisfied("^0.56.0", "0.57.0"), Some(false));
    assert_eq!(
        npm_range_satisfied("^18.0.0 || ^19.0.0", "19.2.0"),
        Some(true)
    );
    assert_eq!(npm_range_satisfied("github:x/y#v1.0.0", "1.0.0"), None);
    assert_eq!(npm_range_satisfied("*", "0.56.0"), None);
    // Operators pass through, spaced or not.
    assert_eq!(npm_range_satisfied("~0.56.0", "0.56.9"), Some(true));
    assert_eq!(npm_range_satisfied("~0.56.0", "0.57.0"), Some(false));
    assert_eq!(npm_range_satisfied("<0.56.0", "0.55.9"), Some(true));
    assert_eq!(npm_range_satisfied(">= 0.54.0 < 1", "0.99.0"), Some(true));
    assert_eq!(npm_range_satisfied("=0.54.0", "0.54.1"), Some(false));
    // Wildcards, hyphen ranges and tags are refused, never read as satisfied.
    for range in ["x", "X", "0.x", "0.56.*", "0.54.0 - 0.56.0", "latest", ""] {
        assert_eq!(npm_range_satisfied(range, "0.56.0"), None, "{range:?}");
    }
}

/// The live chain: the template, the kit tool and the kit's peers (as the
/// tool's lock records them) agree, so a scaffold's `npm install` resolves.
#[test]
fn template_tool_and_kit_peer_pins_agree() {
    let files = react_files();
    let template = &files
        .iter()
        .find(|f| f.path == "package.json")
        .unwrap()
        .content;
    let violations = kit_chain_violations(template, KIT_TOOL_PACKAGE_JSON, KIT_TOOL_PACKAGE_LOCK);
    assert!(violations.is_empty(), "{violations:#?}");
}

/// Each seed breaks exactly one rule of `kit_chain_violations` on a
/// known-good synthetic triple, independent of the live pins.
#[test]
fn kit_chain_checks_redden_on_each_seeded_violation() {
    let deps = r#"{
        "@qontinui/design-kit": "github:qontinui/qontinui-design-kit#v0.2.1",
        "@qontinui/ui-bridge": "0.56.0",
        "@qontinui/ui-bridge-headless": "0.7.0",
        "@qontinui/ui-bridge-wrapper": "0.12.0"
    }"#;
    let template = r#"{
        "name": "my-app",
        "dependencies": {
            "@qontinui/design-kit": "github:qontinui/qontinui-design-kit#v0.2.1",
            "@qontinui/ui-bridge": "0.56.0",
            "react": "^19.2.4"
        },
        "devDependencies": {
            "@qontinui/ui-bridge-headless": "0.7.0",
            "@qontinui/ui-bridge-wrapper": "0.12.0"
        }
    }"#;
    let tool = format!(r#"{{ "name": "tool", "private": true, "dependencies": {deps} }}"#);
    let lock = format!(
        r#"{{
        "lockfileVersion": 3,
        "packages": {{
            "": {{ "name": "tool", "dependencies": {deps} }},
            "node_modules/@qontinui/design-kit": {{
                "version": "0.2.1",
                "resolved": "git+https://github.com/qontinui/qontinui-design-kit.git#0123456789abcdef0123456789abcdef01234567",
                "peerDependencies": {{
                    "@qontinui/ui-bridge": ">=0.54.0 <1",
                    "@qontinui/ui-bridge-headless": ">=0.7.0 <1",
                    "@qontinui/ui-bridge-wrapper": ">=0.12.0 <1",
                    "react": "^18.2.0 || ^19.0.0"
                }}
            }}
        }}
    }}"#
    );
    assert!(kit_chain_violations(template, &tool, &lock).is_empty());

    // Rewrite one JSON value (by pointer) in a literal; the pointer must hit.
    let edit = |text: &str, pointer: &str, value: Option<&str>| -> String {
        let mut json: serde_json::Value = serde_json::from_str(text).unwrap();
        match value {
            Some(v) => {
                *json
                    .pointer_mut(pointer)
                    .unwrap_or_else(|| panic!("seed pointer {pointer} misses")) = v.into();
            }
            None => {
                let (parent, key) = pointer.rsplit_once('/').unwrap();
                // A JSON-pointer token escapes `/` as `~1` and `~` as `~0`.
                let key = key.replace("~1", "/").replace("~0", "~");
                let removed = json
                    .pointer_mut(parent)
                    .and_then(|p| p.as_object_mut())
                    .and_then(|o| o.remove(&key));
                assert!(removed.is_some(), "seed pointer {pointer} misses");
            }
        }
        json.to_string()
    };
    let reddens = |violations: Vec<String>, needle: &str| {
        assert!(
            violations.iter().any(|v| v.contains(needle)),
            "expected a violation containing `{needle}`, got {violations:#?}"
        );
    };
    let kit_peer =
        "/packages/node_modules~1@qontinui~1design-kit/peerDependencies/@qontinui~1ui-bridge";

    // Rule 1: the template's kit tag differs from the tool's.
    reddens(
        kit_chain_violations(
            &edit(
                template,
                "/dependencies/@qontinui~1design-kit",
                Some("github:qontinui/qontinui-design-kit#v9.9.9"),
            ),
            &tool,
            &lock,
        ),
        "rule 1 (kit ref)",
    );
    // Rule 2: each template pin drifts from the tool's, in either section.
    for (pointer, pin, needle) in [
        (
            "/dependencies/@qontinui~1ui-bridge",
            "0.53.0",
            "@qontinui/ui-bridge Some(\"0.53.0\")",
        ),
        (
            "/devDependencies/@qontinui~1ui-bridge-headless",
            "0.6.0",
            "@qontinui/ui-bridge-headless Some(\"0.6.0\")",
        ),
        (
            "/devDependencies/@qontinui~1ui-bridge-wrapper",
            "0.11.0",
            "@qontinui/ui-bridge-wrapper Some(\"0.11.0\")",
        ),
    ] {
        let violations = kit_chain_violations(&edit(template, pointer, Some(pin)), &tool, &lock);
        reddens(violations.clone(), "rule 2 (ui-bridge pins)");
        reddens(violations, needle);
    }
    // Rule 2: the tool's pin is a range, not an exact version.
    reddens(
        kit_chain_violations(
            template,
            &edit(&tool, "/dependencies/@qontinui~1ui-bridge", Some("^0.56.0")),
            &lock,
        ),
        "rule 2 (ui-bridge pins): the kit tool pins @qontinui/ui-bridge `^0.56.0`, not an exact version",
    );
    // Rule 3: the lock's root dependencies drifted from the tool's.
    reddens(
        kit_chain_violations(
            template,
            &tool,
            &edit(
                &lock,
                "/packages//dependencies/@qontinui~1ui-bridge",
                Some("0.54.0"),
            ),
        ),
        "rule 3 (stale lock)",
    );
    // Rule 4: the locked kit is a different version than the tool's tag.
    reddens(
        kit_chain_violations(
            template,
            &tool,
            &edit(
                &lock,
                "/packages/node_modules~1@qontinui~1design-kit/version",
                Some("0.0.1"),
            ),
        ),
        "rule 4 (lock kit entry): @qontinui/design-kit locked at version Some(\"0.0.1\")",
    );
    // Rule 4: the locked kit resolved to a tag, not a sha.
    reddens(
        kit_chain_violations(
            template,
            &tool,
            &edit(
                &lock,
                "/packages/node_modules~1@qontinui~1design-kit/resolved",
                Some("git+https://github.com/qontinui/qontinui-design-kit.git#v0.2.1"),
            ),
        ),
        "does not end in a 40-hex sha",
    );
    // Rule 5: the exact live defect — an exact 0.54.0 peer against a 0.56.0 pin.
    reddens(
        kit_chain_violations(template, &tool, &edit(&lock, kit_peer, Some("0.54.0"))),
        "rule 5 (kit peer): the kit's peer @qontinui/ui-bridge `0.54.0` rejects the pinned `0.56.0`",
    );
    // Rule 5: the peer entry is missing.
    reddens(
        kit_chain_violations(template, &tool, &edit(&lock, kit_peer, None)),
        "rule 5 (kit peer): the locked kit records no peer for @qontinui/ui-bridge",
    );
    // Rule 5: the peer range is unparseable.
    reddens(
        kit_chain_violations(template, &tool, &edit(&lock, kit_peer, Some("github:x/y#v1"))),
        "rule 5 (kit peer): the kit's peer @qontinui/ui-bridge `github:x/y#v1` (or the pin `0.56.0`) cannot be checked",
    );
}

/// The template's UI Bridge pins must never fall below the runner's own
/// pins for the same packages. Both are read at test time — the runner's
/// from the repo-root `package.json` — so the two cannot drift apart
/// again (the template once sat at `^0.21.1` while the runner shipped
/// `^0.53.0`) without this test going red.
#[test]
fn template_ui_bridge_pins_are_not_below_the_runners_own() {
    let files = template_files(TEMPLATE_REACT_UI_BRIDGE, "My-App", "my-app").unwrap();
    let template_pkg: serde_json::Value = serde_json::from_str(
        &files
            .iter()
            .find(|f| f.path == "package.json")
            .unwrap()
            .content,
    )
    .expect("template package.json is valid JSON");

    let runner_pkg_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../package.json");
    let runner_pkg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&runner_pkg_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", runner_pkg_path.display())),
    )
    .expect("runner package.json is valid JSON");

    for name in ["@qontinui/ui-bridge", "@qontinui/ui-bridge-auto"] {
        let template = dep_range(&template_pkg, name)
            .unwrap_or_else(|| panic!("template package.json does not pin {name}"));
        let runner = dep_range(&runner_pkg, name)
            .unwrap_or_else(|| panic!("runner package.json does not pin {name}"));
        assert!(
            range_floor(&template) >= range_floor(&runner),
            "template pins {name} {template}, below the runner's own {runner}"
        );
    }
}

#[test]
fn version_helpers_read_the_forms_the_template_uses() {
    assert_eq!(version_floor("^0.53.0"), Some((0, 53, 0)));
    assert_eq!(version_floor("0.7.0"), Some((0, 7, 0)));
    assert_eq!(version_floor("~1.2.3"), Some((1, 2, 3)));
    assert_eq!(version_floor("github:a/b#v0.1.0"), None);
    assert_eq!(version_floor("^1.2"), None);
    assert!(is_git_tag_ref("github:qontinui/qontinui-design-kit#v0.1.0"));
    assert!(!is_git_tag_ref("github:qontinui/qontinui-design-kit#main"));
    assert!(!is_git_tag_ref(
        "github:qontinui/qontinui-design-kit#abc1234"
    ));
    assert!(!is_git_tag_ref("^0.1.0"));
}

#[test]
fn unknown_template_is_rejected() {
    assert!(template_files("angular", "x", "x").is_err());
}

#[test]
fn project_type_and_manifest_mapping() {
    assert_eq!(project_type_for_template(TEMPLATE_REACT_UI_BRIDGE), "react");
    assert_eq!(project_type_for_template(TEMPLATE_EMPTY), "empty");
    assert_eq!(
        manifest_for_template(TEMPLATE_REACT_UI_BRIDGE),
        "package.json"
    );
    assert_eq!(manifest_for_template(TEMPLATE_EMPTY), "README.md");
}
